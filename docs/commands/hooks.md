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
libra hooks codex    {session-start|prompt|tool-use|permission-request|compaction|stop|session-end|subagent-start|subagent-end}
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

The installer appends a hidden, bounded `--capture-budget-ms` argument to
each managed command: it derives the value from the provider handler timeout
while retaining up to 1000ms for terminal pending-finalizer cleanup (a
one-second legacy handler retains a usable `500ms` capture slice; the defaults
are Claude `10s → 9000` and Codex ordinary events `30s → 29000`). Codex
`SessionEnd` is capped by its host at `3s → 2000`, even when another requested
Codex timeout is longer. It is not a public tuning flag. Re-run `libra agent
enable --agent <provider>` to refresh an older hook configuration; the runtime
converts the value to one dual-clock deadline before reading hook stdin,
bounds its database waits, and never re-anchors a terminal receipt's original
wall-clock deadline. Opening the repository database works as for every other
repository command: pending schema migrations are applied first (each version
atomically, lock waits bounded by the hook's database slice), and a database
written by a newer Libra is refused with the path-free `LBR-IO-001`
newer-binary remedy listed below. The budget bounds killable-helper, database, and other
deadline-aware phases; it is not a strict end-to-end wall-clock guarantee for
the current per-hook process. Source classification, canonicalization, secure
open, and rewind occur synchronously before descriptor handoff, so an NFS/FUSE
stall cannot be forcibly cancelled.

Only a trusted `SessionEnd` creates the durable pending-artifact handoff used
for autonomous recovery. A later `SessionStart` checks the bounded indexed
queue and may launch a detached, source-free recovery worker; the hook itself
does not wait for that worker. Other lifecycle events, including turn-level
`Stop`, do not create that artifact. Recovery therefore preserves the latest
eligible terminal snapshot, not every intermediate turn: a provider that does
not redeliver an interrupted turn can leave that turn without a checkpoint.
`libra agent doctor --repair` remains the foreground, inspectable recovery path.
Candidates that cannot be authenticated or matched to their current catalog
receipt, are superseded, already belong to a quarantined session, or have
exhausted their original retry budget are retained in quarantine and removed
from automatic replay. Transient database/deadline failures and a live writer
attempt leave the candidate pending for a later hint. Quarantined evidence
remains inspectable with `libra agent doctor`; an explicit repair attempt is
available only under the original terminal-receipt policy.
The worker selects candidates from its current worktree; artifacts belonging
to another worktree remain pending for a hint from that worktree. Workspace
identity and lease fence are part of the same selection boundary, so an
artifact owned by another workspace in the same worktree also remains pending.
The bounded pending/quarantine retention limit is shared across worktrees in
the repository, so a full queue requires `libra agent doctor` inspection before
additional terminal artifacts can be retained.

While a terminal snapshot is retained as an unpublished recovery artifact, a
native redelivery of the same `SessionEnd` exits non-zero with a content-free,
retryable message instead of capturing again: it does not reread the provider
source or reserve coverage, and completion belongs to the `SessionStart`
worker or `libra agent doctor --repair`. Coverage claims held by a retained
artifact are not taken over by later writers until that replay completes or
the session is explicitly erased, so later checkpoints of a resumed session
skip those turns and an import of the same session reports partial coverage.
A trusted `SessionEnd` delivered from a working directory that differs from
the session's recorded working directory exits non-zero as explicitly
incomplete: no artifact is retained and no recovery worker is hinted.
Non-terminal events from such a directory remain advisory.

Current Claude and Codex command handlers also carry the provider-supported
`statusMessage: "libra capture"` marker. During refresh or disable, Libra only
claims a handler when that marker, the direct absolute command-path grammar,
the forwarded verb, and the handler timeout/budget pair all match. An
unmarked command — whether bare, standard-named, or renamed — is never owned
by disable. An explicit enable may adopt an unmarked matcher-less direct
absolute legacy or canonical command only when its executable exactly equals
the selected binary; it rewrites the command with the marker, after which
disable can remove it.

Codex records hook approval by matcher-group and handler position. When a
Libra-only group before a later user group is removed, Libra retains an empty
placeholder so the user's existing approval remains at the same position. If a
matcher-less group mixes a Libra-managed and user handler, enable and disable
refuse before changing either `hooks.json` or `config.toml`: move the handlers
into separate groups (or remove the Libra one manually), then re-approve the
remaining user hook.

Codex enable/disable treats `$CODEX_HOME/hooks.json` and `config.toml` as one
coupled update. Libra serializes its own commands with a `$CODEX_HOME` lock,
prepares both files before publishing either, and detects edits it observes
before each replacement. Do not edit either file while `libra agent enable` or
`disable --agent codex` is running: arbitrary editors cannot participate in a
portable atomic compare-and-replace. If Libra reports a non-plaintext integrity
recovery-journal path after a concurrent-edit error, leave that journal in
place, resolve the edit manually, then remove the reported journal before
retrying. It contains limited transaction metadata (schema version, operation,
phase, and a recovery fence) plus domain-separated file fingerprints, never
raw settings snapshots; an unknown fingerprint state is deliberately zero-write
rather than an automatic repair.

Managed Session Capture currently requires a Unix host to initialize its
repository-private deduplication key with secure descriptor-relative,
no-replace publication. On non-Unix platforms capture refuses before creating
any key file and reports an actionable diagnostic instead of weakening that
boundary. This fixed capability result is never silent for Codex: a
nonterminal `libra hooks codex` callback (and the legacy `libra agent hooks
codex` alias) prints the path-free Unix-host remedy to stderr and exits `0`,
while `SessionEnd` returns non-zero with the same remedy. The fail-closed
installed Claude surface and the hidden `libra agent hooks` Claude Code /
OpenCode alias return non-zero for every event with that same path-free remedy
and `LBR-UNSUPPORTED-001`, never a generic "retry the hook" message. That narrow
capability exception does not relax the legacy alias's fail-closed handling of
malformed envelopes. It applies only to callbacks invoked inside a Libra
repository: a callback outside any Libra repository follows the
outside-repository contract below on every platform, Unix or not.

Codex callbacks are advisory to the host agent until a trusted terminal
boundary exists. A malformed/unbound callback and a nonterminal capture
failure emit only a sanitized tracing warning and exit successfully; there is
no trusted session/scope on which a terminal receipt could be written. Once a
scope-bound `SessionEnd` has been validated, Libra must either persist or
observe durable completion/pending recovery evidence. If it cannot do so
(for example, a bounded database lock or finalizer failure), it exits
non-zero rather than silently acknowledging a lost terminal boundary. A
successful terminal retry may remain pending for `libra agent doctor` or may
complete normally; narrow nonterminal checkpoint/maintenance failures can
also leave a non-sensitive retryable diagnostic on an existing session.

A callback invoked from a working directory outside any Libra repository has
no scope to bind and is never a trusted terminal boundary. The installed Codex
surface acknowledges it and exits `0`, including `SessionEnd`, without
creating repository state. The installed Claude surface and the hidden
`libra agent hooks` alias return the fixed, path-free repository-not-found
error (`LBR-REPO-001`, exit 128) for a well-formed frame; a malformed frame is
still rejected first with `LBR-AGENT-008`. A damaged active repository (for
example an unreadable linked-worktree `commondir`) is not treated as outside
a repository: it stays a trusted failure, and the fail-closed surfaces
(installed Claude, the hidden `libra agent hooks` Claude Code / OpenCode /
Codex alias, and both gemini entries) report the same fixed, path-free stable
codes the repository preflight of every other command uses, never the generic
`LBR-INTERNAL-001` capture failure:

| Damaged active repository | Stable code (exit 128) | Remedy in the message |
|---------------------------|------------------------|-----------------------|
| Storage cannot be resolved (detached, migrating or corrupt linked worktree) | `LBR-REPO-003` | from the main worktree run `libra worktree repair --confirm <worktree-path>` (or re-add the worktree) |
| Repository database missing | `LBR-REPO-002` | restore the repository's `.libra` storage |
| Repository database cannot be opened | `LBR-IO-001` | install a newer Libra if a newer one wrote it, otherwise restore repository storage |
| `core.objectformat` cannot be read | `LBR-IO-001` | repair the repository database |
| `core.objectformat` is unsupported | `LBR-REPO-002` | repair `core.objectformat` |

Precedence is: a malformed frame (`LBR-AGENT-008`) and an invocation outside
any repository (`LBR-REPO-001`), then the non-Unix capability refusal
(`LBR-UNSUPPORTED-001`), then these repository classes, then the generic
capture failure. The installed Codex surface keeps its own exit policy: a
nonterminal callback is acknowledged (exit `0`) and a trusted `SessionEnd`
exits non-zero — with the same repository code and remedy for a damaged active
repository, and with its generic terminal diagnostic otherwise.

Malformed ingress is never durable: the installed Claude surface and hidden
`libra agent hooks` alias reject invalid size / UTF-8 / JSON / schema /
reported-working-directory / transcript-path frames with `LBR-AGENT-008`
(exit 128) without echoing stdin. Both path-like fields are capped at 4096
bytes before a capture scope or store is opened.
The installed Codex surface preserves its host-safe acknowledgement policy
(exit 0), but emits only a sanitized diagnostic and creates no session or
checkpoint for an invalid frame.

`libra hooks gemini <verb>` no longer ingests: gemini is uninstall-only
(AG-17), so stale hook configs installed before the demotion get an
actionable error pointing at `libra agent remove gemini` instead of
silently capturing data. Outside any Libra repository it (and the hidden
`libra agent hooks gemini` alias) returns the fixed repository-not-found
error (`LBR-REPO-001`, exit 128) instead, and in a damaged active repository
the path-free `LBR-REPO-003` / `LBR-REPO-002` / `LBR-IO-001` classes listed
above (the database is opened as every repository command opens it: pending
migrations are applied and a newer-Libra schema is refused).

## Read-only / immutable installations

Hook entries are exempt from Libra's auto-upgrade machinery (issue #502).
A hook host may run with the Libra installation on a read-only filesystem
(immutable container, CI sandbox, read-only mount), where the auto-upgrade
startup recovery gate would fail to take `<install-dir>/.libra-upgrade.lock`
and leak an `auto-upgrade recovery could not complete` warning into the
host's hook diagnostics. `libra hooks <provider> <event>` therefore skips
both the startup recovery gate and the `upgrade.mode=auto` check: it never
attempts to create `.libra-upgrade.lock`, emits no auto-upgrade warning,
and adds no auto-upgrade work to the configured hook budget. Normal (non-hook)
commands are unaffected and keep the existing auto-upgrade behavior — a
crashed install transaction is still recovered by the next regular command.

Hook entries (including the hidden `libra agent hooks` alias) are dispatched
immediately after argument parsing, so they also bypass three other steps of the
ordinary command path:

- **Operation recording.** A hook callback is never recorded as an operation:
  it does not appear in `libra op log` and is never a `libra op undo` /
  `libra op restore` target. Its capture state is recorded in the capture
  catalog (`agent_session` / `agent_checkpoint`, see `libra agent session list`
  and `libra agent checkpoint list`) and on `refs/libra/traces`. Operation
  views still snapshot that ref, but `libra op undo` / `op redo` / `op revert`
  / `op restore` keep `refs/libra/traces` and the other Libra-owned capture and
  history branches (`intent`, `libra/intent`) at their current values instead
  of rewinding them to the view. Checkpoints captured after the restored view
  therefore stay reachable from the ref, and later callbacks extend the same
  chain.
- **Global configuration schema policy.** A global config store written by a
  newer Libra never blocks a callback, and the dispatcher prints no schema
  warning for it. A callback that writes checkpoint objects can still print the
  storage layer's single fallback warning (the newer global storage config is
  ignored and local storage is used).
- **Object-index repair replay.** A callback does not replay pending durable
  object-index repair markers; the next ordinary repository command does.
  The repository database itself is still opened with pending schema
  migrations applied and a newer-Libra schema refused, as described above.

Capture failures stay observable without exposing hook payloads: the Codex
path records a non-sensitive `agent.hook.ingest` tracing warning with a
closed-vocabulary reason. A retryable catalog diagnostic is attempted only
for later narrow failure paths that already own a capture reservation;
generic ingress failures do not fabricate a session just to store a
diagnostic. A trusted `SessionEnd` that cannot create its required evidence
is deliberately surfaced to the provider instead of being hidden by this
advisory policy. See the hook invocation observability work tracked as CX-03
in `docs/development/plan/plan-20260904.md`.

To enable capture, run `libra agent enable --agent <name>` for a
supported roster agent; this installs the provider hook config.
To disable capture, run `libra agent disable --agent <name>`.

## Providers and Events

The provider taxonomies intentionally differ. The command verbs below are the
stable targets written by the installer; several native provider event names
share a single capture verb.

### Claude Code

Claude Code recognizes the following lifecycle verbs:

| Verb | Trigger |
|------|---------|
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

### Codex

Codex has eleven native event names, collapsed to nine command verbs. It has
no `ModelUpdate` event: configuring `model-update` for Codex is unsupported
and can be skipped as an unknown callback. `PermissionRequest` and the two
native sub-agent boundaries are Codex-specific capture events.

| Codex event name(s) | Installed command verb |
|---------------------|------------------------|
| `SessionStart` | `session-start` |
| `UserPromptSubmit` | `prompt` |
| `PreToolUse`, `PostToolUse` | `tool-use` |
| `PermissionRequest` | `permission-request` |
| `PreCompact`, `PostCompact` | `compaction` |
| `Stop` | `stop` |
| `SessionEnd` | `session-end` |
| `SubagentStart` | `subagent-start` |
| `SubagentStop` | `subagent-end` |

Each AgentTraces event reads its provider-specific JSON payload from stdin,
passes validated ingress, and persists only its redacted catalog/checkpoint
projection through the capture coordinator. `AgentTraceEvent` JSONL remains
the separate legacy `HookTarget::AiIntent` compatibility path; installed
Claude/Codex hooks do not append that JSONL event as their capture store.
Codex acknowledges malformed/unbound callbacks and advisory nonterminal
capture failures; a trusted `SessionEnd` persistence failure is non-zero so a
terminal boundary cannot be silently lost. Claude keeps its validation
failure status. Provider hooks use the installer-owned bounded deadline and
apply it to their killable-helper and other deadline-aware stages. As noted
above, pre-handoff synchronous host filesystem work can outlast that budget, so
operators must not treat it as an end-to-end hard timeout.

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
    "SessionStart": [{"hooks": [{"type": "command", "command": "<absolute-libra-path> hooks claude session-start --capture-budget-ms 9000", "timeout": 10, "statusMessage": "libra capture"}]}],
    "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "<absolute-libra-path> hooks claude prompt --capture-budget-ms 9000", "timeout": 10, "statusMessage": "libra capture"}]}],
    "PreToolUse": [{"hooks": [{"type": "command", "command": "<absolute-libra-path> hooks claude tool-use --capture-budget-ms 9000", "timeout": 10, "statusMessage": "libra capture"}]}],
    "PostToolUse": [{"hooks": [{"type": "command", "command": "<absolute-libra-path> hooks claude tool-use --capture-budget-ms 9000", "timeout": 10, "statusMessage": "libra capture"}]}],
    "Stop": [{"hooks": [{"type": "command", "command": "<absolute-libra-path> hooks claude stop --capture-budget-ms 9000", "timeout": 10, "statusMessage": "libra capture"}]}],
    "SessionEnd": [{"hooks": [{"type": "command", "command": "<absolute-libra-path> hooks claude session-end --capture-budget-ms 9000", "timeout": 10, "statusMessage": "libra capture"}]}]
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
| `0` | Codex event recorded, skipped, or acknowledged after malformed/unbound ingress, a callback outside any Libra repository (including `SessionEnd`), or an advisory nonterminal capture failure; other providers may also return success after a normal skip |
| `1` | Reserved for ordinary non-hook CLI failures; malformed hook input does not use this code |
| `128` | A non-Codex hook rejected malformed ingress with `LBR-AGENT-008`, was invoked outside a Libra repository (`LBR-REPO-001`), found a damaged active repository (`LBR-REPO-003` / `LBR-REPO-002` / `LBR-IO-001`), encountered another fatal capture error, or a trusted Codex `SessionEnd` could not persist durable recovery evidence, was a redelivery of a still-retained terminal artifact, or arrived from a working directory other than the session's recorded one |
