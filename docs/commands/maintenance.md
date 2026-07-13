# `libra maintenance`

Run tasks to optimize Git repository data.

## Synopsis

```
libra maintenance <subcommand> [options]
```

## Description

The `maintenance` command runs a set of scheduled maintenance tasks that
help keep a Libra repository efficient and healthy. It is modelled after
Git's `git maintenance` command (introduced in Git 2.29).

Tasks can be run individually or all at once. A `--dry-run` mode is
available to preview what would be changed without performing any writes.

## Subcommands

### `run`

Run one or more maintenance tasks.

```
libra maintenance run [--task <task>] [--dry-run] [--quiet]
```

**Options**

- `--task <task>` — Task to run (may be given multiple times). Defaults to all tasks.
- `--dry-run` — Report what would be done without making any changes.
- `--quiet`, `-q` — Suppress progress output.

**Supported tasks**

| Task | Description |
|---|---|
| `gc` | Remove unreachable loose objects after recursively tracing SQLite refs/reflogs (including annotated-tag targets), every index stage, every file-backed stash reflog entry, and held merge/rebase autostash sidecars; malformed/unreadable roots or reachable objects fail closed before deletion |
| `loose-objects` | Pack old loose objects into a new pack file |
| `pack-refs` | Collapse individual ref files into `packed-refs` |
| `incremental-repack` | Repack existing pack files |
| `commit-graph` | Write a Git-compatible v1 commit-graph file (incl. octopus merges via the EDGE chunk, and SHA-256 repositories with 32-byte OIDs + a SHA-256 trailer) |
| `prefetch` | Prefetch remote refs (requires remote config; skipped) |

### `register`

Register the current repository for periodic maintenance.

```
libra maintenance register [--schedule <schedule>]
```

- `--schedule <schedule>` — Cron-like schedule expression (default: `hourly`).

### `unregister`

Unregister the current repository from periodic maintenance.

```
libra maintenance unregister
```

### `status`

Show whether this repository is registered for maintenance.

```
libra maintenance status
```

## Examples

Run all maintenance tasks:

```
libra maintenance run
```

Run only garbage collection:

```
libra maintenance run --task gc
```

Preview what would be done:

```
libra maintenance run --dry-run
```

Register the repository with a daily schedule:

```
libra maintenance register --schedule=daily
```

Show registration status as JSON:

```
libra --json maintenance status
```

## See Also

- [`libra gc`](./gc.md) (not yet implemented)
- [`libra fsck`](./fsck.md)
