# `libra automation`

Manage AI automation rules for the current repository.

## Synopsis

```bash
libra automation list
libra automation run [--rule <id>] [--now <rfc3339>] [--live]
libra automation history [--limit <n>]
```

## Description

`libra automation` reads the repository automation configuration, evaluates
cron-style rules, and records execution history in the repository database.
By default, `run` uses a dry-run executor so shell actions are planned and
recorded without spawning external commands. Pass `--live` only when the
configured actions should actually run.

**Linked worktrees**: every `libra automation` subcommand fails closed in a
linked worktree until the unified Code/Agent configuration resolver lands —
`automations.toml` would be read from the linked worktree's local gitdir,
where no repository configuration lives, so the commands would silently see
an empty rule set. VCS-event dispatch (commit/add/branch/switch/push triggers) is
likewise disabled in linked worktrees, with one warning per command rather
than a silent skip. Run from the main worktree instead.

## Subcommands

| Subcommand | Description |
|------------|-------------|
| `list` | Validate and list configured automation rules |
| `run` | Run due rules, or one named rule with `--rule <id>` |
| `history` | Show recent automation run history |

## Options

| Flag | Subcommand | Description |
|------|------------|-------------|
| `--rule <id>` | `run` | Run a single rule regardless of whether its trigger is due |
| `--now <rfc3339>` | `run` | Evaluate due rules against a simulated current time |
| `--live` | `run` | Execute shell actions that pass safety preflight |
| `--limit <n>` | `history` | Number of history rows to show; defaults to `20` |

## Human Output

`list` prints one tab-separated row per rule:

```text
<rule-id>	<trigger-kind>	<action-kind>
```

`run` prints one tab-separated row per result:

```text
<rule-id>	<status>	<message>
```

`history` includes the finished timestamp:

```text
<finished-at>	<rule-id>	<status>	<message>
```

## JSON Output

`--json` uses command-specific envelopes:

- `automation.list`
- `automation.run`
- `automation.history`

Example:

```json
{
  "ok": true,
  "command": "automation.run",
  "data": {
    "results": []
  }
}
```

## Examples

```bash
# Validate the rules in .libra/automation.toml and list them
libra automation list

# Plan due rules without running them (dry-run is the default for run)
libra automation run

# Run a single rule by id regardless of whether its cron trigger is due
libra automation run --rule my-rule

# Simulate a specific current time when evaluating cron triggers
libra automation run --now 2026-05-23T12:00:00Z

# Actually spawn shell actions that pass safety preflight
libra automation run --live

# Show the 50 most recent automation history rows
libra automation history --limit 50

# Structured JSON envelope for agents
libra automation --json list
libra automation --json run
```

The same banner is rendered by `libra automation --help` so the doc and the
CLI surface stay in sync (cross-cutting `--help` EXAMPLES rollout, see
`docs/development/commands/_general.md` item B).

## Notes

- The command requires a Libra repository for `run` and `history` because
  history is stored in `.libra/libra.db`.
- Configuration validation errors are surfaced before rules run.
- `--live` is intentionally explicit; dry-run remains the default.
