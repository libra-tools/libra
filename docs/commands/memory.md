# `libra memory`

Search and diagnose repository-local development-history Memory compiled from
Libra Intent, Task, Run, session, decision, evidence, and code-version records.

## Synopsis

```bash
libra memory search <query> [filters] [--limit <n>]
libra memory show <note-id> [--revision <oid>] [--evidence]
libra memory status
libra memory rebuild [--dry-run]
```

## What it reads

Each result is a structured Episode: a bounded summary of one Task or Intent
iteration. The Episode stores claims, outcomes, code anchors, and typed evidence
references. Raw sessions and tool output remain in their existing history; they
are resolved only when `show --evidence` requests them.

`search`, `show`, `status`, and `rebuild --dry-run` are read-only. A plain
`rebuild` replaces only SQLite's rebuildable Memory projection. It does not move
the repository Memory ref or modify authoritative Memory objects.

## Search

Search uses SQLite FTS5 with `bm25()` ranking, followed by structured filters
and code-version applicability checks.

| Option | Meaning |
| --- | --- |
| `--limit <1..50>` | Maximum returned Episodes; default `10` |
| `--root-kind task\|intent --root-id <id>` | Match one Episode root; the flags must be used together |
| `--intent <id>` | Match Episodes related to one Intent |
| `--task <id>` | Match Episodes related to one Task |
| `--ended-from <RFC3339>` / `--ended-until <RFC3339>` | Bound the Episode end time |
| `--completion completed\|failed\|cancelled` | Match the recorded outcome |
| `--code-change changed\|unchanged\|unknown` | Match whether code changed |
| `--path <path>` | Match one exact Memory taxonomy path |
| `--path-prefix <prefix>` | Match a Memory taxonomy path prefix |
| `--include-diagnostics` | Include path-changed, diverged, and unknown code applicability states |

Normal search output shows the stable note ID, exact revision object ID,
Task/Intent root, outcome, code-change status, code applicability, evidence
reference count, BM25 score, and summary. By default, only Episodes safe to
inject at the current code version are returned.

## Show

`show` resolves the current confirmed revision unless `--revision` pins a
historical revision. `--evidence` authorizes every typed reference against the
authenticated repository identity before resolving it with the same source
bounds and redaction policy used during compilation. Missing, unauthorized,
corrupt, or over-budget fragments appear as omissions instead of being silently
substituted. Human output keeps the Episode compact: root and outcome, time
range, goal, summary, claim groups, code anchors/paths, and evidence counts.

## Status

`status` reports:

- the repository Memory ref;
- projection state, projected ref, and last event sequence;
- compile-job state, pending generation, active/expired lease, retry, and error counts;
- linked SQLite FTS5 capability;
- repository digest-key availability and the current frozen view hash.

It does not print Episode content, prompts, job lease tokens, or raw evidence.
The command checks the current head manifest and SQLite watermarks, then scans
at most 4,096 compile-job rows. JSON exposes `jobs.scan_limit` and
`jobs.truncated`; when `truncated` is true, the job counters describe only the
bounded sample. Use `rebuild --dry-run` when a full authoritative-history
validation is required.

## Rebuild

`rebuild --dry-run` validates the complete authoritative history and reports the
head, event, note, revision, and last-sequence counts without writing SQLite.
`rebuild` then reconstructs the repository-scoped projection and FTS index from
that same history. Use it when `status` reports `stale` or after projection
tables are damaged or removed. If validation encounters corrupt history, the
error reports a bounded damage point (Memory head OID or event sequence/object
IDs) without printing note or evidence content.

## JSON

Use the global `--json` or `--machine` flag. The command names are
`memory.search`, `memory.show`, `memory.status`, and `memory.rebuild` inside the
standard Libra envelope:

```json
{
  "ok": true,
  "command": "memory.search",
  "data": {
    "view_hash": "...",
    "selector_version": "episode-fts-bm25-v1",
    "items": []
  }
}
```

An empty search succeeds with `items: []`. Invalid filters, missing notes,
unavailable FTS5, stale projection, unknown schema, and corrupt history use the
stable `LBR-MEMORY-*` errors documented in
[`docs/error-codes.md`](../error-codes.md). Corruption errors include the same
bounded location as `details.damage_point` in structured output.

Read commands and `rebuild --dry-run` do not migrate SQLite. A regular
`libra memory rebuild` applies known pending migrations before replay. If the
repository schema is newer than the installed Libra build, the command returns
`LBR-MEMORY-002`; upgrade Libra before retrying.

## Examples

```bash
libra memory search "authentication retry"
libra memory search "timeout" --task task-42 --limit 5
libra memory search "parser" --path-prefix episodic.tasks
libra --json memory search "root cause"
libra memory show <note-id>
libra memory show <note-id> --revision <oid> --evidence
libra memory status
libra memory rebuild --dry-run
libra memory rebuild
```

## Scope

The command covers repository-local Task and Intent Episodes. Manual
remember/delete/update operations, Memory revert, consolidation, MCP tools,
team synchronization, and cross-repository search are outside this command.
