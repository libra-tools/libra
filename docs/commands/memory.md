# `libra memory`

Inspect and rebuild Libra's deterministic Agent development-history memory
projection.

## Synopsis

```bash
libra memory status
libra memory list
libra memory show <episode-id>
libra memory rebuild
libra memory status --allow-stale
```

## Description

`libra memory` reads and rebuilds the repository-scoped, zero-authority
projection of durable agent-development facts. Every `memory_episode` row is a
pure function of the repository's committed facts (commit/change, agent
session, agent run, bridge operation), so the projection holds no
authoritative state and can always be re-derived with `libra memory rebuild`
(GC-DM-01).

All subcommands are **read-only** — they never write an `operation` row.

**Freshness (fail-closed, ADR-DM-10):** the projection is only as current as
its last `rebuild`. A new commit, session, or run advances the underlying
facts past the stored fingerprint, making the projection stale. `status`,
`list`, and `show` refuse to answer from a stale projection and exit `128`
with `LBR-MEMORY-001` unless you pass `--allow-stale`. With `--allow-stale`
the JSON envelope marks `"stale": true` and human output prints a banner; with
it unset, the projection is refreshed on the next `libra memory rebuild`.

## Subcommands

| Subcommand | Description |
|------------|-------------|
| `status` | Report projection freshness / horizon state and persisted counters |
| `list` | List the derived memory episodes currently in the window |
| `show` | Render a single derived memory episode |
| `rebuild` | Re-derive the zero-authority projection from the current facts |

## Options

| Flag | Subcommand | Description |
|------|------------|-------------|
| `--allow-stale` | `status`, `list`, `show` | Answer from the projection even when it is stale (ADR-DM-10); marks `"stale": true` in `--json` and prints a banner in human output |
| `--reveal` | `show` | Reveal the full episode body (by default a short placeholder is shown) |
| `--json` | all | Structured JSON envelope (global flag) |

## Human Output

`status` (fresh): each persisted value is printed on its own line.

```text
schema_version:   1
selector_version: 1
rules_version:    1
horizon_truncated: false
revoked_count:     0
aged_out_count:    0
rebuilt_at:        1791541568
```

When the projection is stale and `--allow-stale` is passed, a banner is printed
first:

```text
memory projection is stale
schema_version:   1
...
```

`list` prints one tab-separated row per episode: `episode_id`, `source_kind`,
and `title`.

`show` prints the episode's identity, outcome, and title. `--reveal` includes
the full body; without it, the body is elided.

## JSON Output

`--json` uses command-specific envelopes:

- `memory` (status / list / show / rebuild)

Each envelope carries a `data` object. `status` uses the frozen read contract
(DM-11):

```json
{
  "ok": true,
  "command": "memory",
  "data": {
    "schema_version": 1,
    "stale": false,
    "selector_version": 1,
    "rules_version": 1,
    "horizon_truncated": false,
    "revoked_count": 0,
    "aged_out_count": 0,
    "rebuilt_at": 1791541568
  }
}
```

`list` returns a `data.episodes` array; `show` returns a single episode object;
`rebuild` returns the rebuild report (`projected`, `horizon_truncated`,
`revoked_count`, `aged_out_count`).

## Examples

```bash
# Show projection freshness / horizon state
libra memory status

# List the derived memory episodes in the window
libra memory list

# Show a single derived memory episode
libra memory show <episode-id>

# Rebuild the zero-authority projection (GC-DM-01)
libra memory rebuild

# Answer from a stale projection (fail-closed is the default)
libra memory status --allow-stale

# Structured JSON envelope for agents
libra --json memory status
```

The same banner is rendered by `libra memory --help` so the doc and the
CLI surface stay in sync (cross-cutting `--help` EXAMPLES rollout, see
`docs/development/commands/_general.md` item B).

## Notes

- The command requires a Libra repository because the projection lives in
  `.libra/libra.db` and the repository id is needed to scope episodes.
- `memory rebuild` reads up to `memory.horizon` (default `5000`) commits in the
  branch tip's first-parent chain; `revoked_count` and `aged_out_count` are the
  rows removed during that reconciliation (GC-DM-01).
- The projection never leaks host-absolute paths into `--json` (ER-11); code
  paths are emitted as repository-relative byte strings.
