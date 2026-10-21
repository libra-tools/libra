# `libra agent`

> **Coverage-v2 source version:** The coverage-v2 descriptions below refer to the Libra 0.30.50 source line and later compatible versions. This is not a stable-release announcement; check the [official releases](https://github.com/libra-tools/libra/releases) and `libra --version` for installed support. Other sections retain their own version requirements. Coverage-v2 does not prove native origin or authorize native reasoning artifacts.

Manage external-agent capture for Claude Code, Codex, OpenCode, and Grok.

Grok is supported and hook-installable with `support_wave=grok`, not `first_batch`, and is not a `review` or `investigate` launcher. **Automatic Thinking capture is paused.** Hook lifecycle events are recorded through the shared pipeline; decoding does not read session logs or write `GROK_HOME/libra-capture`. Existing readers for local `updates.jsonl` and `chat_history.jsonl` are not yet connected to the authorized snapshot/checkpoint writer. Offline observer JSON is scratch state, not a Libra checkpoint. The intended transcript retains Thinking text and unproven `encrypted_content`; Libra does not decrypt it. Current synthetic reader tests do not prove that live checkpoints contain it. Default `checkpoint show` does not print Thinking plaintext; an explicit transcript path returns only content actually stored.

## Native Windows capture

This support is available in **current main source builds** and is not included
in the **v0.30.50 release binaries**. Until a published release includes it, use
the [source build instructions](../../README.md#install).

Windows builds support Session Capture on local NTFS drives, without WSL.
Install **OpenCode 2.x** (the GitHub 1.19.0 CLI has an incompatible plugin API),
and place its configuration in `%USERPROFILE%\.config\opencode\opencode.json`.
The native CLI is available from
<https://opencode.ai/update/api/latest/cli/opencode/opencode-windows-x64.zip>.
In PowerShell, with `libra.exe` and `opencode.exe` on PATH:

```powershell
libra init
libra agent add opencode
libra agent rpc trust --dir C:\path\to\verified-opencode2
libra agent rpc trust opencode
opencode run --standalone "Reply with a short greeting."
libra agent session list
libra agent checkpoint list
libra agent checkpoint export <checkpoint-id>
```

Windows validates local paths with no-follow `CreateFileW` handles and rejects
reparse points (including junctions), alternate streams, device names, and UNC
capture storage. Repository-private keys use a current-user/SYSTEM ACL,
`LockFileEx` exclusion, and atomic no-replace publication. A missing or damaged
key is never silently replaced during authentication or recovery.

The Windows OpenCode exporter runs **without an OS filesystem/network sandbox**.
Only an explicitly trusted `opencode.exe` can run; its bytes and file identity
are revalidated, and the executable is held against replacement. A Windows Job
Object contains the process tree, with a 30-second deadline and a 16 MiB accepted
output limit. Output is held in an ACL-protected temporary file and removed
after reading; stderr and ambient credential variables are not forwarded.
The size is checked while the process runs and after it exits; Windows has no
Unix `RLIMIT_FSIZE` write-time limit, so temporary output can exceed the accepted
limit between checks. Linux bubblewrap and macOS Seatbelt requirements are
unchanged. Windows capability availability requires trusted exporter bytes;
it does not claim that an OS sandbox is present.

Windows checkpoint listing and ordinary transcript export are available.
Owner-only raw reasoning artifact export and the object/findings scans in
`libra agent doctor` remain unavailable on Windows; doctor reports these scans
as unavailable and applies no repairs to them.

## OpenCode ordinary capture on macOS (Libra 0.30.49)

Use the official OpenCode **2.0.29** for ordinary automatic capture. The frozen
2.0.26 vocabulary and private native producer sections below describe separate
historical/explicit native lanes; they are not prerequisites for ordinary capture.

Libra 0.30.50 adds session-directory filtering, native executable selection
behind supported package-manager launchers, and an explicit warning for empty
metadata-only checkpoint exports. The corresponding behavior below requires
Libra 0.30.50 or later.

From the repository you want to capture, check the installed versions and prepare
its exporter trust and observer:

```bash
libra --version
opencode --version
libra agent rpc trust --dir /absolute/directory/containing/opencode
libra agent rpc trust opencode
libra agent add opencode
libra --json agent list --schema-version 2
libra agent doctor
```

Use the real directory containing the reviewed executable. The current CLI takes
directory registration and binary trust as two separate commands; it does not
accept a binary filename as the `opencode` slug or combine `--dir` with that slug.
Run OpenCode from this repository after installing or refreshing the observer;
`--pure` / `OPENCODE_PURE=1` disables its capture plugin. After a captured turn,
inspect and export the stored checkpoint:

```bash
libra agent session list
libra agent checkpoint list
libra agent checkpoint show <checkpoint-id>
libra agent checkpoint export <checkpoint-id> -o capture.json
```

An authorized source with unsupported reasoning state may save its safe ordinary
copy as `Unproven` / `Partial`. This includes prompt, model, token projections,
readable reasoning text, and unproven reasoning fields. Unsupported reasoning
state and timing are written into that copy.
It grants no
native artifact, Complete coverage claim or native ACK. Any original `originProof`
or sealed candidate remains outside this fallback. Historical imports containing
unsupported reasoning state still fail with `LBR-AGENT-020`; ordinary capture
support does not enable that import or durable partial replay. Real native
`signature`, `redactedData` and `reasoningEncryptedContent` source and archival
remain separate gates; Source A remains UNPROVEN. OpenCode/account/model checks
run only locally on macOS, outside CI.

## Synopsis

```bash
libra agent status
libra agent list [--schema-version <1|2>] [--json]
libra agent import (--session <id> | --path <path> | --since <rfc3339> | --all) [--agent <name>] [--limit <n>] [--cursor <n>] --yes
libra --json agent graph <session> [--repo <path>]
libra agent enable [--agent <name>]...
libra agent add [<name>...]
libra agent disable [--agent <name>]...
libra agent remove [<name>...]
libra agent session <subcommand>
libra agent checkpoint <subcommand>
libra agent skill <subcommand>
libra agent clean [--all]
libra agent doctor [--repair]
libra agent workspace list [--limit <n>] [--cursor <token>] [--state <state>]...
libra agent workspace show <workspace-id>
libra agent push [--remote <name>] [--force-rewrite]
libra agent rpc <subcommand>
libra agent bridge --stdio
libra agent mcp --stdio
```

## Description

`libra agent` manages Libra's external-agent capture surface. It installs and
removes provider hooks, reports captured session/checkpoint state, exposes
read-only diagnostics, and can push `refs/libra/traces` to a remote.

`libra agent workspace list|show` is the read-only machine interface over
the workspace registry (`workspace_record`): every linked worktree, task
worktree, or remote workspace an agent runtime has associated, with its
lifecycle state (`provisioning`/`active`/`releasing`/`released`/
`orphaned`), owner, lease fence/expiry, and canonical path. `list` uses
keyset pagination (`workspace_id` ascending; default `--limit 50`, capped
at 500; round-trip `next_cursor` verbatim) and accepts repeatable
`--state` filters; `show <workspace-id>` returns one frozen
schema-v1 record. Lease mutation is never exposed here — it stays inside
the agent runtime's internal services.

The supported roster is `claude-code`, `codex` and `opencode` (first batch),
and all three are hook-installable: `claude-code` writes `.claude/settings.json`,
`codex` writes user-level `$CODEX_HOME/hooks.json` plus Libra-managed trust
entries in `$CODEX_HOME/config.toml` (untrusted Codex hooks are skipped
silently, so trust entries are part of the install), and `opencode` writes the
Libra-managed plugin `.opencode/plugin/libra-hooks.js` (note: `opencode --pure`
disables all external plugins, including capture).
`gemini` was demoted out of the supported roster and is uninstall-only:
`libra agent remove gemini` removes previously installed Libra-managed hooks
(idempotent), captured sessions stay readable, and `add`/`enable` for it — or
for any other non-roster agent — return an actionable unsupported error.

For Codex, `enable` and `disable` update the coupled `$CODEX_HOME/hooks.json`
and `config.toml` under a cooperative Libra-process lock. They prepare both
files first and reject an edit observed before a replacement, but an arbitrary
editor cannot be covered by a portable atomic compare-and-replace. Do not edit
either file while the command runs. A reported non-plaintext integrity
recovery-journal path means no further automatic overwrite is safe; preserve
that journal, resolve the edit manually, then remove the reported journal
before retrying. It records limited transaction metadata (schema version,
operation, phase, and a recovery fence) plus domain-separated file
fingerprints, never raw hook or config contents; an unknown fingerprint state
is intentionally zero-write rather than an automatic repair.

### OpenCode event input contract

The frozen OpenCode event contract below records the audited **2.0.26** vocabulary;
the current ordinary 2.0.29 baseline is described above and in its dedicated section. Its current
lifecycle inputs are `session.created`, user `session.inbox.delivered`,
`session.execution.succeeded`, `session.execution.failed`,
`session.execution.interrupted`, `session.deleted`, and
`session.compaction.ended`; `tool.execute.after` is a separate observation hook.
Delivery requires `role="user"` and a prompt string, including an empty string.
Only `user`, `superseded`, and `inactivity` interruptions end a turn; `shutdown`
keeps restart continuity. An enqueue is merged by the plugin and is never a
standalone lifecycle input.

Compatibility inputs are `session.status(idle)`, `message.updated`,
`session.compacted`, and `server.instance.disposed` with a tracked session ID.
Busy/retry status frames are rejected. The parser requires a prompt string for
legacy `message.updated`; an empty string and an omitted role are accepted,
and a declared role must be `user`. Neither `message` nor `user_prompt` fills a
missing prompt. The deprecated `session.idle` alias remains accepted, with a
content-free `legacy_event_alias` tracing warning.

The managed plugin uses the OpenCode 2.0.26 `setup(ctx)` API, an event
subscription and an observe-only tool hook. One OpenCode server (for example
`opencode serve --service`) delivers every project's events to every project's
plugin. In Libra 0.30.50 and later, sessions known to belong to another
`location.directory` are not forwarded, and a session moved out of the directory
is closed here. Events without a location are forwarded unless their session
was placed elsewhere. A tool call for a session not yet
placed is attributed through the host's `session.get` (when the host cannot
answer, it is forwarded as before and `session_location_unavailable` is reported
once). Refresh it with `libra agent enable --agent opencode`: status reports old
or edited templates, and duplicate managed copies, as not installed until
refreshed.
Node and Bun use the same `node:child_process` transport with the pinned
absolute Libra filename and `shell:false`. Session/turn boundaries run
synchronously; tool and compaction observations run asynchronously and never
change host arguments or return context/control output.

User inbox enqueue/delivery pairs open one turn per classified inbox ID within
the bounded live history window. Frames also carry a stable `message_id` for
Libra’s existing bounded receipt ledger; this is not an unlimited historical
replay guarantee. Legacy
user messages pair with their first text part, or send an explicit empty string
before the next unrelated event or cleanup. Assistant step events supply the
model. Buffers are bounded: 64 tracked sessions process-wide, 128 pending prompts, 128 early
text parts, 256 classified inbox/message IDs and 16 observation children;
prompts are limited to 16,384 UTF-16 units without a split surrogate. Capacity
or unavailable text produces a fixed, content-free diagnostic at most once per
reason per plugin instance; truncated
prompts carry `prompt_truncated=true`. ID history is a rolling 256-entry window and is pruned when a session ends;
new prompts keep flowing after the window fills. When the session window fills,
the least recently observed session is flushed with `server.instance.disposed`
and the new session is admitted. A later observation can reopen an evicted
session; this closes Libra tracking only and does not delete or stop the OpenCode
session. Early text also uses a rolling window.

Cleanup and process exit flush all tracked sessions using one shared deadline
per cleanup or process exit, respectively. Forwarding is best effort and fails
open for the OpenCode host. Nonzero child exit or timeout emits only a fixed
`forward_failed` / `forward_timeout` diagnostic, at most once per reason per plugin instance. Each child has the current export deadline plus
15 seconds (currently 45 seconds), with `SIGKILL` at timeout. Install checks both
plugin layouts, rejects symlinks and special files before reading, preserves
user-owned files and rolls back its canonical update if managed duplicate
removal fails. These checks assume stable directory names; concurrent directory
relocation and edits after the final ownership check remain deferred.
`opencode --pure` / `OPENCODE_PURE=1` disables all external plugins and hook
capture. Local Node/Bun tests use synthetic events and a controlled exporter;
real OpenCode content capture and native reasoning proof require their separate
acceptance gates.

OpenCode 2.0.26 export metadata uses `user.text`, assistant `model.id`,
`tokens` and successful file tools. The assistant token total is
`input + output + reasoning + cache.read + cache.write`; cached tokens are
`cache.read + cache.write`. Session-level totals are not added again. Classic
`info/parts` exports remain compatible: assistant aggregate tokens take
precedence over deduplicated step-finish details. User model fields are ignored.
Modified files combine successful edit/write/patch paths and snapshot files,
with validated patch move sources and destinations. Output paths are project
relative with forward slashes; location-relative tool paths use the session's
proven location, while snapshots already use the project root. Missing history,
external or invalid paths, unsupported shapes, invalid token numbers and
collection limits produce partial metadata with content-free warnings.
Idle, model/agent switches, synthetic/system/skill messages, shell and compaction
are recognized metadata and do not become human prompts. Internal ID, path and
alias limits also mark partial; at the ID cap later messages are omitted.
Extraction does not read files or archive reasoning content, and synthetic
fixtures do not prove real macOS content capture or native reasoning origin.

The legacy ordinary OpenCode coverage normalizer accepts native 2.0.26 messages and classic
`info/parts`. Each human message starts its own stable logical turn. A valid
native `idle` settles all pending human splits, including failed/interrupted
outcomes; Complete describes captured representation, not task success.
Semantic messages after the last valid idle stay Incomplete. Unknown/malformed
content and reasoning stay Incomplete and are never repaired by idle. The nine
classic metadata parts (step-finish, patch, step-start, snapshot, retry,
compaction, agent, subtask, file) are recognized without semantic projection.
Native user attachments, skill text, and tool file results are unsupported:
they mark Incomplete without copying attachment bytes or URIs into records.
Valid agent/skill metadata, model/usage and provider state do not change the
digest. Native tools use name/id/state.input and text state.content; an error
without content projects only its structured error.message, never response.body.
No reasoning bytes or provider state establish trusted native origin here.

OpenCode user prompts pass through the same reminder filter for native/classic
coverage and native/classic/flat-JSONL metadata extraction. Exact literal
`<system-reminder>...</system-reminder>` blocks, including nested blocks, are
removed wherever they occur; text outside them stays byte-for-byte unchanged.
An injection-only message adds no user prompt or user semantic record. The
filter also runs before skill matching; independent token metadata is retained.
An unfinished block suppresses its tail and marks partial/Incomplete; a stray
closing tag is retained and marks partial/Incomplete. Idle cannot clear these
violations. Only user text is filtered, and other providers keep their existing
semantics. This is a literal marker filter, not proof of producer origin.

OpenCode extraction is fail-visible for empty input, invalid JSON, and illegal
export shapes: `extraction.partial=true` and fixed payload-free warnings are
recorded. Valid independent flat model/token fields remain projected even when
the role is missing or unsupported. A valid empty session export is distinct
from missing transcript bytes (`extraction.present=false`). Noncritical
extraction errors do not block an otherwise admissible checkpoint; malformed
exports do not replace an existing checkpoint. Coverage, authorization, and
native-origin gates remain required.

## Subcommands

| Subcommand | Description |
|------------|-------------|
| `status` | Report captured external-agent session status |
| `list` | List the supported agents with their capability matrix (roster, hooks, install state) |
| `import` | Discover and import historical Claude/Codex transcript files or one trusted, sandboxed OpenCode export after explicit consent |
| `graph <session>` | Inspect the read-only session → turn → revision → subagent capture graph; requires global `--json`/`--machine` (the interactive TUI entry was removed in the W5 breaking release) |
| `enable` | Enable one or more external agents and install hooks |
| `add` | Alias of `enable`: `add <name>` ≡ `enable --agent <name>` |
| `disable` | Disable one or more external agents and uninstall hooks |
| `remove` | Alias of `disable`: `remove <name>` ≡ `disable --agent <name>` |
| `session list` | List captured sessions |
| `session show <id>` | Show a captured session, including a non-sensitive retryable checkpoint-capture diagnostic when the last Codex Stop failed |
| `session stop <id>` | Mark a captured session as stopped |
| `session resume <id>` | Mark a stopped captured session active again |
| `session derive-tool-calls <id>` | Derive tool-call records from a captured session |
| `checkpoint list` | List captured checkpoints |
| `checkpoint show <id>` | Show a safe structural checkpoint summary (metadata and internal object IDs are withheld) |
| `checkpoint rewind <id>` | Inspect or apply a working-tree rewind for one checkpoint |
| `checkpoint export <id>` | Export a checkpoint's transcript. Redacted by default (no authorization); raw (un-redacted) export requires `--allow-raw --raw` and is recorded in the append-only `agent_audit_log` (`LBR-AGENT-013` when refused without it). A checkpoint recorded without a transcript (metadata-only capture — for OpenCode, the export bridge was unavailable or failed when it was written) exports zero bytes and emits a warning |
| `skill search` | Search captured skill events by `--skill`, `--provider`, `--session`, and RFC3339 `--since`/`--until` (keyset-paginated with `--limit`/`--cursor`, `--json`). A read-time projection over checkpoint metadata — no dedicated table |
| `skill list` | Alias of `skill search` (same filters) |
| `skill registry` | Show the curated per-agent discoverable-skill registry (`--provider <slug>` to scope; the public SkillDiscoverer surface) |
| `clean` | Clean up temporary checkpoints from stopped sessions (prune fails closed while a checkpoint write is in flight, the traces ref reaches uncataloged commits, or durable object-index repair remains pending; also drops `object_index` rows made unreachable) |
| `doctor` | Diagnose hook installation and capture state; detect (and with `--repair` fix) checkpoint-store inconsistencies. Its object checks deliberately inspect only this repository's local object directory: unavailable alternate/remote objects are reported conservatively rather than resolving an externally controlled alternate path while producing diagnostics. The read-only `legacy_code_residue` field (human: a "Frozen Code residue" line) reports whether the frozen Code-era paths `.libra/sessions/code/` and `.libra/code/` and the `libra/intent` ref still exist; it never deletes or rewrites that state — cleanup is tracked separately (plan-20260920 ADR-RC-04 / DEFER-RC-02) |
| `push` | Push `refs/libra/traces` to a remote (`--force-rewrite` for the non-fast-forward push after a `clean` prune, using force-with-lease) |
| `rpc list` | List discovered `libra-agent-*` binaries on `PATH` (with trusted/quarantined state); requires the external-agents opt-in |
| `rpc trust <slug>` | Trust a discovered binary — records path + sha256 + device/inode/mtime provenance (refused when its directory is world-writable, or when the binary is not under a trusted directory — `LBR-AGENT-005`). The provider-exporter slug `opencode` instead pins the provider's own CLI binary — resolved only from registered trusted directories, never `$PATH` — for the sandboxed export bridge; this form needs no external-agents opt-in. A mise (aube) or pnpm `.bin` launcher shim is followed to the native executable it declares, which must itself be under a trusted directory (reported as `launcher` in `--json`); a JavaScript launcher is refused (`LBR-AGENT-005`). Provenance is hashed in one streaming pass with no fixed size cap, so large single-file provider CLIs (the OpenCode Bun build is ~171 MiB) stay trustable; a file that yields more bytes than its reported size while being hashed is refused (`LBR-AGENT-005`) |
| `rpc trust --dir <path>` | Register a trusted directory (`agent.external_agents.trusted_dirs`, default `~/.libra/agents`): external binaries are only trustable when their canonical path lives under one. The path is canonicalized and must be an existing, non-world-writable directory |
| `rpc untrust <slug>` | Revoke binary trust; OpenCode also revokes native issuers atomically (see native-origin rotation and revocation below). Always available, even while external agents are disabled |
| `rpc invoke` | Invoke one JSON-RPC method on a trusted `libra-agent-*` binary |
| `bridge --stdio` | Run the repository-scoped DeepSeek Harness bridge over stdin/stdout (JSON-RPC 2.0 NDJSON). The **only** standard inbound write transport for Harness; it is not `libra code --control`. stdout carries exactly one protocol frame per response; diagnostics go to stderr. Protocol v1: 20-method allowlist, 256 KiB frame cap, 64 in-flight requests, 30 s default deadline. All 20 methods are implemented: the `initialize` handshake; session/event ingress `session.open`, `event.append` (batch ack / idempotent / digest-conflict / server-side redaction), `session.flush`, `session.close`, `evidence.append`, `provenance.append`; the read methods `context.get`, `status.get`, `history.search`, `checkpoint.list`, `checkpoint.show` and `diff.get`; the mutations `checkpoint.create`, `commit.create`, `checkpoint.restore` and `review.run` (each with `operation_id` idempotency, actor binding and approval gating); and workspace lease `workspace.claim` / `workspace.renew` / `workspace.release` (owner derived from the authenticated bridge session). `diff.get` takes a closed `mode` (`worktree` / `staged` / `checkpoint`) plus validated repository-relative `paths` — never a free-form revision or pathspec — and forces `--no-ext-diff` / `--no-textconv` so repository config cannot become process execution. `commit.create` commits the current index only (no `-a`, no pathspec, no amend, no author override) and records its association graph in `agent_bridge_link` rather than in the commit message. `checkpoint.restore` requires an explicit `expected_head` fence plus a clean index/worktree and never moves HEAD. `review.run` starts a read-only review and returns its `run_id`; replaying the same `operation_id` reports that run's state instead of starting a second one. A stale HEAD or dirty worktree is refused with `LBR-AGENT-038` before any write. An ack only means the redacted projection is durable, never that the Harness raw transcript has been migrated. Not a Git command. |
| `mcp --stdio` | Serve the repository Model Context Protocol endpoint on stdin/stdout (JSON-RPC 2.0, one message per line). Legacy protocol versions `2025-11-25` and `2025-06-18` answer `initialize`, `notifications/initialized`, `ping`, `tools/list`, and `tools/call`. Protocol failures are JSON-RPC errors (`-32700` parse, `-32600` invalid request or batch, `-32601` unknown method, `-32602` invalid params or unknown tool). A tool argument that fails the tool's input schema is a tool error `LBR-AGENT-027` (`isError: true`), not a protocol error. stdout carries only JSON-RPC messages. This is not `libra agent bridge` and not the removed `libra code` MCP. |

## MCP server

`libra agent mcp --stdio` is the repository MCP server for external clients. The client configuration shape is `command` = `libra`, `args` = `["agent", "mcp", "--stdio"]`, and `"cwd"` set to the repository directory:

```json
{"command":"libra","args":["agent","mcp","--stdio"],"cwd":"/path/to/repo"}
```

The server speaks legacy MCP `2025-11-25` and `2025-06-18`. An unrecognized `protocolVersion` is answered as `2025-11-25`. `capabilities` is only `tools` with `listChanged: false`. JSON-RPC batches are rejected. Input that does not match a tool's schema returns tool error `LBR-AGENT-027`.

Tool results are redacted before they are written. The server redacts `structuredContent`, scrubs control characters other than newline and tab, and rebuilds the text copy from that scrubbed value. Known secret patterns are removed. If that redaction cannot be parsed again, the server returns tool error `LBR-AGENT-034` and does not fall back to the original result. A full JSON-RPC response larger than 256 KiB returns tool error `LBR-AGENT-028`. A read tool that runs longer than 30 s returns tool error `LBR-AGENT-036` with `retryable` set to true. Read tools run in at most 4 concurrent read workers (4 个并发读子进程). On that deadline the server replies and kills the worker before another read can take its slot.

## Common Options

| Flag | Subcommand | Description |
|------|------------|-------------|
| `--agent <name>` | `enable`, `disable` | Select agent names; omit to target the supported roster (`add`/`remove` take the names positionally) |
| `--schema-version <1\|2>` | `list` | Select the machine schema. Version 1 is the frozen legacy row; version 2 adds `methods[]` entries for `transcript_discoverable`, `importable`, and `export_bridge` availability |
| `--session <id>` / `--path <path>` / `--since <rfc3339>` / `--all` | `import` | Select exactly one historical-import scope. `--path` also requires `--agent`; OpenCode supports explicit `--session` through its export bridge |
| `--yes` | `import` | Required for JSON/non-TTY imports; confirms that Libra may read private provider session content, redact it, and write typed projections to this repository |
| `--restore-erased` | `import` | Explicitly remove a local anti-resurrection tombstone and retry import. Requires `--yes` and appends an audit row |
| `--limit <n>` / `--cursor <n>` | `import` | Bounded discovery page (default 20, hard maximum 100) and the next zero-based cursor returned by the preceding page; one invocation also has a 64 MiB cumulative raw-input cap. The per-source cap is `min(agent.max_transcript_read_bytes, 16 MiB adapter hard cap)`; an explicit larger config prints the actual effective cap |
| `--repo <path>` | `graph` | Read capture metadata from another Libra repository instead of discovering from the current directory |
| `--limit <n>` | `session list`, `checkpoint list` | Maximum rows per page (default 50, hard cap 500 — larger values clamp with a stderr note; `0` is treated as `1`) |
| `--cursor <cursor>` | `session list`, `checkpoint list` | Opaque keyset cursor from the previous page's `next_cursor`; do not construct by hand |
| `--extract-transcript <path>` | `session show` | Re-derive and copy at most 16 MiB from the currently verified Claude Code transcript source to a new local file. This never falls back to a captured metadata path, never overwrites an existing path, and rejects oversize sources without publishing output. With `--json`, the result is the safe `{ "session": <session row>, "extracted_transcript": { "output_path": <path>, "bytes": <count> } }` envelope; the extraction block contains only the destination path and byte count; neither JSON nor human output reveals the provider source path. |
| `--all` | `clean` | Clean all stopped-session checkpoints instead of only the most recent |
| `--repair` | `doctor` | Repair detected checkpoint-store inconsistencies (rebuild stale/missing catalog rows, re-enqueue missing `object_index` rows, safely drain valid expired ordinary writer markers, and immediately drain `cleanup_pending` markers regardless of ordinary TTL); malformed markers remain `manual_required`; detection-only when omitted |
| `--remote <name>` | `push` | Select the remote used for pushing agent trace refs |
| `--force-rewrite` | `push` | Allow the non-fast-forward push that follows a local `clean` prune (the traces ref is Libra-managed and rewritten as a whole chain); uses force-with-lease against the last tip this repository pushed — never an unconditional force — so a remote rewritten elsewhere still fails closed |
| `--dry-run` | `checkpoint rewind` | Show the impact without modifying files; this is the default |
| `--allow-raw` / `--raw` | `checkpoint export` | Authorize + request a raw (un-redacted) export; without `--allow-raw` a `--raw` request is refused (`LBR-AGENT-013`) and audited |
| `--justification <text>` / `-o <path>` | `checkpoint export` | Audit justification and output file for a raw export |
| `--gc` / `--retention-days <n>` / `--dry-run` | `clean` | Retention GC across three windows: (1) drop checkpoints from stopped sessions older than `agent.retention.transcript_days` (default 90; override with `--retention-days`); (2) prune reviewer stderr diagnostic logs of terminal review/investigate runs older than `agent.retention.stderr_days` (default 30) while keeping each run's aggregate record; (3) **A0-09** remove whole terminal review/investigate run directories (`findings.md`, `manifest.json`, `state.json`, reviewer logs) older than `agent.retention.findings_days` (default 90). The objectized findings blob is content-addressed and reclaimed by the repo-level `libra maintenance run --task gc` reachability pass (PD-04), which prunes orphaned findings blobs together with their `object_index` rows while live-run manifests keep their OIDs alive (per-run retention never deletes a shared object). Non-terminal/undated runs are skipped fail-safe; `agent_audit_log` is never touched. `--dry-run` reports what each window and companion cleanup *would* remove (including JSON `findings_runs_pruned` and `import_identities_pruned`) without deleting anything |
| `--apply` | `checkpoint rewind` | Restore the working tree for the selected checkpoint |

## Review and investigation workspaces

New review and investigation workspaces live under the repository's common storage at `.libra/worktree/tasks/<run-id>/<attempt-id>/workspace/`. Checkpoint-scoped runs use the sibling `checkpoint-input/` location instead. A continued investigation allocates a fresh attempt; it never overwrites an earlier attempt's workspace. The same common root is used when a run starts from a linked worktree.

Run records, findings and logs stay under `.libra/sessions/agent-runs/<run-id>/`; moving workspace files does not move session metadata or the object store. New run state and manifests use schema version 2 and retain the workspace ownership descriptor. Known version-1 records remain readable without relocating old directories; unknown versions are not rewritten or cleaned automatically.

`agent clean --gc` skips a run whose workspace ownership cannot be verified, whose state or manifest cannot be parsed, or whose workspace removal fails, then continues with other eligible runs. Its text and JSON reports include `findings_runs_skipped` and bounded `findings_gc_warnings` so the refused run can be inspected and retried. A legacy run with `checkpoint-input` inside its run metadata directory remains eligible once all other ownership checks pass; a versionless legacy manifest remains readable.

Libra's internal task sandbox API can bind a verified attempt and limit writes to its workspace and cache paths, with sensitive repository metadata read restrictions. `review` and `investigate` launch external agent CLIs in the isolated copy and do not apply that internal OS sandbox to those child processes; the external CLI's own sandbox settings determine its filesystem access. `libra agent doctor` reports unknown or unowned directories in the managed task container without deleting them.

## Scoped checkpoint resume

`review --checkpoint <id>` and `investigate start --checkpoint <id>` materialize only proven ordinary checkpoint files, read-only, under `.libra/worktree/tasks/<run-id>/<attempt-id>/checkpoint-input/`. Every materialization, including `investigate continue`, revalidates the saved spec against the explicit repository catalog and the checkpoint manifest/tree roles before creating the new attempt input. Supported wrapped checkpoints without a manifest remain compatible. An old full-list spec may include a closed `reasoning/encrypted/<64 hex>` entry only when its path and blob id exactly match the manifest and tree; that entry is excluded without opening its payload. Altered entries, unknown paths, repeated paths and ordinary role mismatches are refused. A saved id that is not a blob, or whose bytes do not hash to that id, is refused before creating the new attempt input. A catalog transport failure leaves a paused continue's state and existing input unchanged. A review setup deadline ends as the existing infrastructure error; cancellation ends as cancelled. Setup has its own deadline and does not deduct time from later reviewers. Filesystem work cooperatively checks the caller's deadline and cancellation, and its owner finishes before returning. Ordinary payload limits remain 64 MiB per file and 256 MiB total. The materialization limits and stable error codes are unchanged.

## Scoped run cleanup

`libra review clean` and `libra investigate clean` remove only finished runs of their own kind. Cleanup verifies each saved workspace descriptor and removes the owned task workspace or checkpoint input before deleting run metadata. A cleanup failure keeps the ownership and retry record; a swapped directory, link, unknown descriptor version or ownership mismatch is refused. Directory names alone do not authorize deletion. Both `--run` and `--all` check lifecycle metadata in the run's own directory before touching read-only checkpoint input. The bulk sweep keeps foreign, unfinished, unknown and corrupt runs; its `removed` count includes only successful deletions. A specifically named run with corrupt or missing state can be removed only when its own valid terminal manifest proves the run id, kind and schema. A readable foreign or unfinished state, or conflicting metadata, is refused. `investigate clean` retains its existing run lock. Bounded input cleanup refuses linked roots or metadata and oversized input; a failure returns an actionable error and does not report a deletion. Keep the run and inspect the reported links, limits or filesystem access before retrying. If I/O or cancellation interrupts cleanup after it starts, some input may already have been removed; the failed operation does not imply rollback. A proved own-kind terminal state remains cleanable when its matching manifest has no terminal marker after an interrupted state-first finalization. Identity/schema/common-field conflicts and differing non-null terminal values remain refused. Bulk qualification I/O, link or size refusals return an error; already-removed earlier runs are not rolled back. A proved own-kind terminal state also remains cleanable when its manifest is missing. A terminal review infrastructure failure may have no reviewer outcome rows while its manifest retains the requested reviewers; that supported shape is cleanable. An unparseable manifest is still refused even beside a proved terminal state: preserve the run and repair its metadata before retrying. This does not add cleanup flags or stable error codes.

## JSON Output

Subcommands that support structured output use the global `--json` and
`--machine` envelope. For example:

```bash
libra --json agent status
libra --json agent list
libra --json agent graph <session>
libra --json agent checkpoint list
libra --json agent rpc list
```

`agent list --json` carries a stable `schema_version` plus one row per
supported agent — the first-batch roster `claude-code`, `codex` and
`opencode`. Unsupported agents (`gemini`, `cursor`, `copilot`, `factory-ai`)
stay registered so historical sessions remain readable, but they are omitted
from the listing. Each row carries `slug`, `agent_kind`, `stability`,
`supported`, `support_wave`, `registered`, `transcript_readable`,
`hook_installable`, `installed`, `launchable_review`, `launchable_investigate`,
`external_binary`, `config_paths`, `protected_dirs`, `capabilities`. The row
shape is a frozen contract for automation.
Claude Code advertises `capabilities.transcript_preparer=true`: after Libra
securely opens and pins an authorized transcript descriptor, it may briefly
wait for a trailing JSONL record to finish flushing through that same
descriptor. The wait and tail probe are bounded; the preparer never reopens a
provider path.

Request `agent list --schema-version 2 --json` only when the caller understands
the extension. Its `methods[]` array reports support and current availability
for transcript discovery, historical import, and the OpenCode export bridge;
the default version 1 payload remains shape-compatible and never gains those
fields implicitly. OpenCode reports `transcript_discoverable` unsupported
because batch discovery is unavailable; explicit-ID `importable` and
`export_bridge` availability depend on its trusted offline exporter/sandbox.
On Unix, the exporter subprocess and its descendants run with both soft and
hard `RLIMIT_CORE` set to zero. This suppresses core files for intentional
limit enforcement and unexpected exporter crashes when the system core
handler honors that limit; signal events may still appear in system logs.
Libra retains the exporter exit status and bounded stderr diagnostics. This
child-only setting leaves the parent Libra process and system configuration
unchanged.

The default OpenCode export budget is 30 seconds; the export lease and plugin
forwarding timeout derive a 15-second margin (45 seconds). Run
`libra agent add opencode` again to refresh an older installed plugin. Schema v2
`importable` and `export_bridge` require a trusted exporter and a successful
platform sandbox probe on Unix: Seatbelt on macOS, the existing trusted bwrap probe on
Linux. Windows requires the trusted native exporter and uses the Job Object
boundary described above, without an OS sandbox. Availability reports those prerequisites; it does not prove that OpenCode
boots, that a session exports successfully, or that reasoning fields have a
trusted native origin. Timeout still rejects partial output and preserves the
existing metadata-only fallback; the 16 MiB export cap is unchanged.

On macOS, OpenCode export requires a **Required Seatbelt server/export pair**.
Both processes deny `process-fork`, use private HOME/XDG roots and a pinned
binary copy, and communicate only on one authenticated loopback port.
Cancellation and the absolute deadline terminate their owned process groups;
Libra never signals a group after reaping its leader. Trust, containment,
readiness, output-cap or shutdown failure remains **metadata-only**, with no
unsandboxed fallback. To enable the exporter, register the directory containing
the verified
`opencode` binary and then pin it: `libra agent rpc trust --dir <path>`
followed by `libra agent rpc trust opencode`. Neither step opens the
external-RPC surface (`agent.external_agents.enabled` stays untouched); the
binary is resolved only from registered trusted directories, never `$PATH`.
The sandbox maps only the pinned file, so it must be the native OpenCode
executable. When the trusted directory holds a package-manager launcher —
the `.bin/opencode` shim written by mise's npm backend (aube) or by pnpm —
`rpc trust opencode` pins the native executable that shim declares, provided
that executable's own directory is also registered (the refusal names the
exact `rpc trust --dir` to run); a JavaScript launcher (`#!/usr/bin/env node`)
is refused. A trust record that still pins a launcher reports `export_bridge`
unavailable. Register the native executable's directory if needed, then re-run
`libra agent rpc trust opencode`.
Linux content export additionally requires a trusted `bwrap` that passes
Libra's descriptor-native `--bind-fd` safety probe. Unsupported or legacy
`bwrap` is reported unavailable and fails closed to metadata-only capture;
install or upgrade the system `bwrap` package rather than enabling an
unsandboxed fallback.
Claude/Codex discovery and import are reported unavailable when the platform
cannot provide Libra's secure provider-root file-open primitive.
Unsupported schema versions fail as a usage error (exit 129, category `cli`)
with `LBR-AGENT-017`.

`agent graph --json` emits the frozen capture-graph schema version 1. Its `data`
object contains exactly `schema_version`, `state`, `session`, `turns`, and
`subagents`. Present sessions expose only the session id, agent kind, state,
and timestamps. Indexed turns expose their logical key, derived zero-based
ordinal, coverage schema/completeness/current revision, checkpoint id,
source channel, and append-only revision history. A whole-transcript
checkpoint may appear under several turns; this shared evidence is never
hidden as superseded. Pre-coverage captures are returned as
`coverage_state="unindexed"` checkpoint chronology without invented revision
facts. Subagent nodes expose only checkpoint/link structure and retain
`resolved` or `unresolved` explicitly.

In Libra 0.30.50 and later, graph keeps
separate rows and revision histories for coverage versions 1 and 2, even when
they share a logical turn key or checkpoint. This does not change the public
JSON schema version 1. An unsupported stored coverage version fails the query
with `LBR-AGENT-009`, without returning a successful graph or candidate content.
Use a Libra release that supports the stored version and run `libra agent doctor`
to inspect the catalog; do not rewrite version numbers or delete evidence.
Doctor reports each version's conflict separately, without raw identities or
candidate content, and leaves conflict resolution manual, including with
`--repair`. These coverage-v2 reader guarantees require Libra 0.30.50 or later.

The JSON/machine graph query never opens transcript or object blobs and never
selects working directories, descriptions, metadata JSON, redaction reports,
or coverage digests. **Breaking change (W5-08):** the interactive
capture-graph TUI was removed in the W5 breaking release — its bounded,
redacted content previews (event counts and compact user/assistant message
previews read from the linked checkpoint, at most 256 KiB each) went away with
it, and the frozen JSON v1 schema above never carried them. A locally erased
session succeeds with `state="erased"`, a null session, empty turns, and
unavailable subagents; it is not recreated. An id absent from both the session
catalog and erasure tombstones fails with `LBR-AGENT-021`. Without global
`--json` or `--machine`, `libra agent graph` exits with a usage error and a
migration hint (exit 129, `LBR-CLI-002`) before reading any capture state.

`agent import` has its own schema version 1 result with `results`, `skipped`,
`partial_results`, `failures`, and `next_cursor`. Every item has one status:
`imported`, `noop`, `partial`, `skipped`, or `failed`. `results` contains only
fully completed selections; discovered cross-repository or erased candidates
are reported under `skipped` with a hashed session id and stable reason code,
while the same condition for an explicit selector remains a failure. A failed selection that made durable turn progress
is reported under `partial_results` and is never included in `succeeded`. A batch that commits some selections but cannot import all
of them exits non-zero with `LBR-AGENT-018`; the structured error details keep
the successful summaries and a per-selection failure list whose session ids
are hashed, preserve `schema_version`, and preserve a nullable `next_cursor`
instead of coercing `null` to zero. Single-selection ownership, cwd, erased, and source-authorization
failures retain `LBR-AGENT-015`, `016`, `019`, and `020` respectively.
Provider timestamps (and derived turn times) more than five minutes past the
importing host's clock are rejected before they can influence session
chronology: that selection fails with the fixed message `the transcript
contains timestamps beyond the permitted clock skew` (`LBR-AGENT-018`) and
writes no durable state.
Successful and partial summaries report parent turn writes in
`checkpoints_written` and independently discovered child-content writes in
`subagent_checkpoints_written`; a child-only mutation is `imported`, not
`noop`. A platform warning that secure child discovery is unavailable does not
make an otherwise complete parent import fail, because it is not evidence that
child content exists.

Historical import is repository-scoped and fail-closed. Libra requires one
unambiguous transcript `cwd`, resolves its Libra storage, and imports only when
it is the current repository. A sibling linked worktree is valid because it
shares that canonical Libra storage; a different repository is not. File sources must stay under the selected
provider's protected root and are opened once with descriptor-relative
no-follow semantics on Unix. Provider roots are opened component-by-component,
and batch discovery opens each Claude source and every Codex year/month/day
component relative to pinned directory descriptors before consent, so a root,
nested directory, or source-file symlink cannot escape the provider root. Platforms without an
equivalent secure open fail closed. Only typed coverage-v1 user/assistant/tool records are serialized,
after field-level redaction. Raw provider envelopes, provider-home source
paths, and unknown fields are not persisted; the verified repository
`working_dir` remains the documented compatibility exception. Replays are idempotent; an incomplete turn
may advance to one complete revision without changing the checkpoint's
structural repository parent. If a different complete payload later claims
the same logical turn, the claim is parked as `conflicted` and Libra retains
exactly the first challenger in `agent_coverage_conflict`: its typed canonical
payload is redacted before persistence and stored with its digest, source
channel, observation time, and deterministic redaction report. Later
challengers do not replace that first evidence; raw provider envelopes and
secret-shaped matched bytes are never stored. The incumbent revision remains
append-only and current until an operator resolves the conflict. Local session erasure writes a durable
anti-resurrection tombstone before deleting the catalog; automatic discovery
and in-flight writers cannot rebuild it. `--restore-erased --yes` is the only
local bypass and is audited.

New V2 imports retain a source only as the repository-keyed, domain-separated
`source/hmac-v2/<64 lower-hex>` commitment. New V2 subagent content records
use their separately domain-separated
`source/subagent-hmac-v2/<64-lower-hex>` commitment. A raw source locator or
an unkeyed source SHA-256 never enters import or subagent catalog metadata,
claims, markers, diagnostics, or cloud records. The optional V2 snapshot
digest is a distinct-domain, repository-keyed
`source/hmac-v2/<64 lower-hex>` commitment over the redacted snapshot content;
the helper's transient SHA-256 is never durable. A bare or tagged unkeyed
SHA-256 is immutable legacy evidence only and cannot be introduced into V2 or
cloud state. V1 rows are readable proof, not a conversion shortcut: Libra
migrates them atomically only after the exact scoped proof matches and the
identity, catalog, and any repair marker are committed and quiescent. Active,
partial, or repair-pending V1 state remains V1 and never mints a parallel V2
record.

Before the first content read/export, interactive confirmation identifies the
selected agent scope, current-repository-only boundary, candidate count/limit,
redaction write, and the fact that a later `libra agent push` may upload the
redacted traces. `--yes` acknowledges only that privacy disclosure; it does
not relax source-root, repository, size, deadline, or platform checks. Import
batch processing is best-effort across sessions and reports exact durable
per-session progress when a later turn fails. The 64 MiB batch budget is
charged from bytes actually read through the held descriptor for successful,
malformed, unauthorized, and oversized candidates alike, including file
growth after authorization. If bounded child discovery fails before it can
return a trustworthy byte count, Libra conservatively charges that child's
entire remaining per-source allowance; retries therefore cannot turn a helper
failure into a batch-cap bypass. The 120-second absolute deadline starts before
discovery and is checked before and during helper phases, traversal, parsing,
reservation, object building, and CAS persistence. Libra does not cancel an
SQLite commit after it starts:
it checks the deadline immediately before each commit, then observes that
commit to a definite outcome. A fully committed final turn is therefore
reported as success even if the clock crosses the deadline while the commit
result is being observed; every uncommitted lease/marker is abandoned on a
deadline failure. If that recovery transaction itself fails, the command
chains the cleanup error and an actionable `agent doctor --repair` hint instead
of reporting only the original failure.
Discovery and authorized-file read helpers are private, kill-on-timeout
processes. The reader consumes the exact descriptor already opened by the
parent; its control wire has fixed safe bounds and commitments, never a
locator, path, provider session value, or raw bytes. Those helper phases share
the absolute deadline. The current command process still classifies,
canonicalizes, securely opens, and rewinds a source synchronously before that
handoff. A stalled NFS/FUSE operation there cannot be forcibly cancelled, so
the 120-second setting is not a strict end-to-end wall-clock deadline. Child
discovery receives a shorter sub-deadline that reserves time for the
independently valid parent checkpoint. Parent-side helper response decoding and
completeness validation recheck that sub-deadline while walking the bounded
response, so a late large child result degrades the parent to partial evidence
instead of suppressing its checkpoint.
After consent, the held-descriptor preparation phase performs parsing and
redaction in its killable helper and cannot reopen a source locator. Source-root
authorization and the initial secure open occur before that handoff. Checkpoint
loose-object writes and the commit/tree reads used while splicing the traces ref
retain their own bounded paths. This preserves descriptor-bound raw reads, but
does not yet provide a strict deadline, FD-only wire, and autonomous replay
after timeout in one per-command/per-hook process; that combination requires a
long-lived owner or a provider ABI that supplies a pre-authorized descriptor,
as recorded in the [Session Capture plan](../development/plan/plan-20260924.md).
Object reads accept only the requested `commit`/`tree` type, verify the full OID
and declared length, and enforce a 16 MiB inflated-payload cap before
allocation, so a hostile compressed object cannot turn bounded helper work into
unbounded memory use.

Before advancing to the next batch candidate, import waits until the completed
candidate's queued object-index writes have reached SQLite. If that drain
exhausts the command deadline, the completed candidate is reported as partial;
the not-yet-started next candidate is never charged with that progress.
Terminal background update errors are barrier failures too, not log-only
successes. Import acquires a session-wide durable repair barrier before
checkpoint writes. Its owner, generation, and lease serialize concurrent
processes; only that exact generation may retire it, and an index failure may
downgrade only the exact import identity/fence that produced the result.
Timeout or update failure marks the barrier repair-pending and the exact
identity partial. A process crash leaves its active generation until the lease
expires; takeover then repairs before writing. Replay runs an idempotent, killable foreground repair of
the marked session's complete E4 object set before it may return `noop`. The
helper holds the SQLite writer slot while it revalidates the session and erase
tombstone, updates the index, and preserves the owned barrier, so repair cannot
reinsert cloud-eligible rows after concurrent erasure. A successful candidate
then retires only its own generation. If bounded repair cannot complete, run
`libra agent doctor --repair` and retry.

Each potentially new OID is recorded as a provisional preclaim in the durable
attempt marker before its loose-object write. It becomes deletion-eligible
ownership only after this writer wins the no-clobber publish and durably moves
the OID into the marker's `created_oids` set. A crash in between is deliberately
leak-safe: an unresolved preclaim is never deleted. Loose objects are compressed into a unique
file in the shared private `objects/info/libra-tmp` directory and promoted without overwrite; an
already-present final path is decompressed and byte-validated before reuse.
A bounded scavenger examines at most 64 entries in that private directory and
removes only regular files older than 24 hours whose names exactly match
`.<40-or-64-lowercase-hex-oid>.tmp-<decimal-pid>-<uuid>`; unrelated files and directories are
retained. File and directory fsyncs are performed only with `--sync-data` or
`LIBRA_SYNC_DATA`; no-clobber atomic publication is always enforced.
Before deleting unreachable `object_index` rows, `agent clean` acquires the
repository-wide repair-marker generation fence, revalidates every candidate
OID, and holds the fence through the prune transaction. A marker published
after the earlier command preflight therefore makes cleanup fail closed instead
of allowing its delayed queue update to resurrect a deleted row. With
`--sync-data`, retiring a repair marker also fsyncs its containing directory.
Append, failure finalization, and erase do not run a repository-wide
reachability drain. A rejected append durably marks its exact generation
`cleanup_pending`; `agent doctor --repair` performs bounded root-fenced
ownership retirement immediately without
waiting for the ordinary writer TTL. A same-session cleanup job makes erase
refuse until that maintenance retires the job; erase never runs the drain
itself. Inline recovery never
unlinks a shared loose object or deletes its `object_index` row: worktree-index
writers do not share the SQLite lock, so physical reclamation is left to
repository GC and its grace/locking/reachability policy. Doctor snapshots refs,
reflogs, every registered worktree index, active sequencer state, and marker/catalog
state outside the SQLite writer transaction, then revalidates the complete snapshot
under the final ownership-retirement transaction. It does not traverse unrelated
object history because this path performs no payload or object-index deletion.
Every marker registration also carries a random writer `generation`; object
preclaims, ownership finalization, the final ref CAS, and marker cleanup all
compare that generation so an expired writer cannot adopt or delete a
same-checkpoint takeover marker.
Root collection is limited to 250,000 reference/reflog rows. Registered index reads
run in a killable helper under a 30-second aggregate deadline;
registry/index files are opened no-follow/nonblocking, required to be regular,
read once from the held descriptor with a `limit + 1` growth check, and parsed/checksummed
from those exact bytes. They are limited to 256 files and 64 MiB aggregate. Hitting any bound is a
fail-closed deferral: candidate ownership remains durable for diagnosis and a
later retry; a completed drain retires attempt ownership while leaving orphaned
payload reclamation to repository GC.
Zero-progress provisional sessions are reaped from their persisted
`import_provisional` flag after lease takeover, and live/export failures after
marker registration release their claims/job lease and clear only ordinary
markers (never a `cleanup_pending` job).

`agent clean --gc` physically deletes terminal, ownerless import identities
after their final coverage state is pruned, including zero-checkpoint identity
rows. It does not reset them to a replayable `discovered` state. Dry-run uses a
rolled-back coverage simulation so `import_identities_pruned` matches the
identical real GC run without mutating catalog state.
Conflict evidence follows its claim rather than becoming an independent
retention root. Erasing a session or pruning its final coverage claim cascades
the retained challenger away; dry-run simulates the same deletion. When a
checkpoint-history rebuild/prune rewinds the current claim to an older
surviving revision, stale challenger evidence is deleted and the claim returns
to its non-conflicted committed state.

The tombstone is propagated. `libra cloud sync` publishes it to D1 under the
generation fence and removes the erased session's mirrored catalog rows, and
`libra cloud restore` is tombstone-first in BOTH directions: it filters rows
carrying a remote tombstone AND rows matching this repository's own
`agent_import_tombstone`, inside the restore transaction. Erasing locally and
then restoring from a mirror that has not yet seen the tombstone therefore
does not bring the session back.

What is still deferred is R2 physical payload deletion: the erased content's
objects remain in R2, so another machine that has never seen the tombstone can
still fetch them. Treat `agent erase` as irreversible for THIS repository, not
as a cross-machine guarantee that every copy is gone.

Private capture recovery evidence is repository-only. Local erasure first
commits the anti-resurrection tombstone, then prunes checkpoint history, and
finally deletes attributable recovery headers/chunks and private session
aliases with the catalog row in one transaction. Deletion uses local
catalog-PK/incarnation ownership, not a recovery MAC or an evictable receipt:
a lost key does not prevent attributable erasure. Foreign or ambiguous corrupt
evidence is retained, without exposing its contents; a lost-capacity diagnostic
is logged after the deletion commits. Such retained evidence still consumes
capacity and may prevent new capture associations. Without a consistent
original repository DB backup, that capacity is lost: doctor/retry is not a
force-discard facility. A DB-copy backup contains sensitive alias associations
and is not anonymized; restoring an old full DB backup can undo local deletion
and its tombstone. Normal receipt completion removes the matching artifact and
only an alias with no remaining reference, in the same transaction.

If pending and quarantine headers collide on one checkpoint key, cleanup
removes only headers attributed to this session. Unknown/foreign headers and
their shared chunks are retained unchanged; explicit erase reports retained
capacity and does not certify that every private copy has been removed.

An unreadable session incarnation, or known aliases from a different
incarnation of that PK, refuses the final deletion: restore consistent
session/alias metadata and retry; the first-phase tombstone remains in effect.
One retained undecodable header blocks every new or re-delivered recovery
artifact and destructive GC/root-walking maintenance for the repository, not
just its own slot. Completion may retain aliases while that header prevents
proving them unreferenced; explicit attributable erasure is still possible.
Out-of-band over-capacity states read at most 17 headers and 17 associations:
unexamined evidence, possibly this session's own content, may remain. The
warning is not proof that every private copy was deleted.

`agent session list --json` and `agent checkpoint list --json` return one
page per call: `data` carries a `schema_version`, the rows under `sessions`
/ `checkpoints` (per-row shape unchanged), and `next_cursor` — an opaque
token to pass back via `--cursor`, `null` once the listing is exhausted.
Pages are ordered newest-first (`started_at` / `created_at` descending,
with the row id as tiebreaker).

The human `agent session list` table renders `started_at` as a relative age
against the current machine clock (for example, `2 hours ago`). JSON output
keeps the raw Unix timestamp for automation.

`agent checkpoint show <id>` is intentionally not a metadata dump. Its default
human and JSON output contains only the fixed checkpoint summary:
`checkpoint_id`, the closed `scope` vocabulary, Unix `created_at`, and whether
a parent snapshot was recorded. It never reads or renders `metadata.json`, a session identifier,
source locator or commitment, redaction detail, or catalog object identifier. Use the explicit checkpoint export path for transcript
content, which applies its own authorization and redaction policy; do not
depend on default `show` output for internal metadata.

Each checkpoint row carries a `scope`. `committed` checkpoints are written at
turn/session boundaries (`Stop` / `SessionEnd`) and carry the redacted
transcript snapshot. There are two deliberately distinct `subagent` evidence
types. Codex `SubagentStart` / `SubagentEnd` hooks create empty-transcript
**boundary** checkpoints linked structurally through `parent_checkpoint_id`.
Claude `<session>/subagents/*.jsonl` files create independent **content**
checkpoints whose `parent_checkpoint_id` remains null. New content claims use a
repository-keyed, domain-separated HMAC V2 commitment
(`source/subagent-hmac-v2/<64-lower-hex>`) derived only after the source is
securely opened and its root, storage binding, scope, and workspace fence have
been revalidated. Append-only revisions select one current content leaf without
persisting the local project slug or filename and without rewriting physical
history. Historical V1 `source/sha256/...` claims, revisions, and checkpoint
metadata remain immutable, read-only evidence; a matching legacy claim is not
silently shadowed by a new V2 row and must stay on its recovery/doctor path.
Claude does not emit a stable boundary identifier,
so its content normally remains `link_state=unresolved`; `agent doctor` reports
that fact and never guesses a match. A provider-stable identifier links content
to a unique boundary through a separate association row, without changing the
checkpoint's immutable traces commit. Both evidence types remain listable,
showable, exportable, prunable, and doctor-visible. An empty or metadata-only
child file has no normalized turn evidence and is therefore recorded as
`partial`, never as a complete child transcript.

Codex capture installs all currently documented lifecycle events: `SessionStart`,
`SessionEnd`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`,
`PostToolUse`, `PreCompact`, `PostCompact`, `Stop`, `SubagentStart`, and
`SubagentStop`. The checkpoint event projection preserves provider correlation
fields such as `turn_id`, `tool_use_id`, `agent_id`, `agent_type`, compaction
trigger, permission mode, tool input, and tool response. Codex's
`encrypted_content` / encrypted-reasoning payload is deliberately dropped from
the capture projection: hooks have no decryption key, and Libra does not retain
an encrypted placeholder or plaintext internal reasoning in this path.

Durable checkpoint metadata keeps only fields Libra can classify safely. Live
hook capture drops the provider-supplied `model`, `source`, `tool_name`, and
session-reference (transcript pointer) fields before anything is persisted, so
consumers of `checkpoint export` output or `refs/libra/traces` sidecars must not
expect them. In a hook-captured checkpoint, `metadata.json` always records
`"model": "unknown"`, the `events/lifecycle.jsonl` lines never carry `model` or
`tool_name` and record `"source": null`, and Codex subagent boundary metadata
records a null `subagent.tool` / `subagent.source`. A hook-captured lifecycle
line's `provenance.hook_event_name` is the canonical snake_case lifecycle kind
(for example `tool_use` or `turn_end`, matching `kind`), not the provider's
native event name. `events/lifecycle.jsonl` is schema v2: every line carries a
mandatory `identity_scheme` (`native_replay_hmac_v2`,
`fallback_action_hmac_v1`, `generic_lifecycle_uuid_v5`, or `import_uuid_v5`)
that declares how its `event_id` was derived; it is a declaration, not a
verifiable replay credential.

Cloud mirroring publishes the capture catalog as dependency-ordered batches
(`session → checkpoint → revision → link → claim`) under a token-fenced
generation. Session, checkpoint, link, and mutable claim projections have independent
monotonic sync generations, so pruning a current child leaf cannot create an
equal-generation conflict and pruning a retained traces chain cannot be undone
by a stale clone. An explicit `--restore-erased` import starts a new durable
replication incarnation for both the session and opaque child-source namespace;
this avoids key reuse while only R2 physical payload deletion remains deferred; D1 tombstone propagation is already active. The generation also binds a canonical digest of the
remote object index; sync rechecks every checkpoint object in D1 and R2, and
restore accepts the catalog only when the manifest and object-index digest are
unchanged before and after the read. Current clients use versioned v2 remote
session/checkpoint tables. Once v2 is activated, D1 triggers reject legacy
unfenced writers with an upgrade error rather than allowing them to invalidate
a coherent restore snapshot.

## Claude Code hook event registry

Libra keeps one Claude Code hook-event registry. A name is safely installable when empty stdout and exit 0 do not take over the host action. `WorktreeCreate`, `WorktreeRemove`, and `PreModelSwitch` are control-sensitive and are not installed. `SessionStop`, `ModelUpdate`, and `Compaction` are deprecated aliases and are not in the current installable set. An unknown event name or an unknown field is partial, and the diagnostic does not include the payload. `libra agent enable` writes every safely installable event. `SessionEnd` has no timeout. `FileChanged` has no matcher and does not emit `watchPaths`. `libra agent doctor` lists each known event as installable or excluded, and whether a Libra-owned entry is configured. `libra agent hooks claude-code observe` accepts one JSON hook frame on stdin, records safely installable events through the existing capture coordinator, and prints nothing. Controllable success and failure both exit 0. `WorktreeCreate`, `WorktreeRemove`, and `PreModelSwitch` are not installed and produce no path or decision. MessageDisplay keeps at most 128 batches per session across hook processes. Later batches write no capture revision and only raise a dropped count, which session erase removes. Requires Libra 0.30.66 or later. This does not claim that every known event is installed.

An authorized Claude Code transcript keeps a non-empty `thinking` string in the ordinary snapshot and in the coverage projection. `thinking.signature` and `redacted_thinking.data` stay in the ordinary transcript bytes and are labeled Unproven when they are not native proof. An empty thinking string is not a missing key, and an empty shell is not a successful read. The coverage schema version stays 1 for turns without reasoning and 2 when a readable thinking record is present. Session metadata and redaction reports do not include the thinking text. `libra agent checkpoint show --json` and the ordinary checkpoint export return that thinking text and `thinking.signature` for an authorized Claude checkpoint, labeled unproven. `checkpoint list`, `session list`, and `agent graph` do not print the body. Live capture, import, and snapshot recovery read those attached fields from the original JSONL, with provider, session, turn, record, and content index. A duplicate key rejects that record and leaves retained bytes in place. A mismatched session file is not associated. Ordinary retention does not create a native artifact. An explicit `libra agent doctor --repair` appends one coverage revision for an authorized legacy Claude checkpoint. The revision key binds that checkpoint and the ordinary transcript content. Repeating the repair with the same content leaves the revision and the stored objects in place. Erase and the cloud mirror keep that replacement together with the old checkpoint reference, and a tombstone blocks revival.

## Examples

```bash
# Show captured-session counts and recent checkpoint summary
libra agent status

# Show the agent capability matrix (supported roster, hooks, install state)
libra agent list

# Negotiate the versioned import/export method matrix
libra agent list --schema-version 2 --json

# Import one historical Claude Code session after explicit privacy consent
libra agent import --session <provider-session-id> --agent claude-code --yes

# Import a bounded page of Codex rollouts modified since a timestamp
libra agent import --since 2026-07-01T00:00:00Z --agent codex --limit 20 --yes --json

# Read one captured session's turn/revision/subagent structure as frozen JSON v1
libra --json agent graph <session-id>

# Read the same graph safely in automation or from another repository
libra --json agent graph <session-id> --repo /path/to/repo

# Enable Claude Code capture and install its hooks (alias of enable)
libra agent add claude-code

# Enable Claude Code capture and install its hooks
libra agent enable --agent claude

# Enable every supported agent at once
libra agent enable

# Disable Claude Code capture and uninstall its hooks (alias of disable)
libra agent remove claude-code

# Remove legacy gemini hooks (uninstall-only channel; idempotent)
libra agent remove gemini

# Disable Claude Code capture and uninstall its hooks
libra agent disable --agent claude

# List captured sessions
libra agent session list

# Show a session and copy its currently verified Claude Code transcript source
libra agent session show <session-id> --extract-transcript /tmp/session.jsonl

# Stop a captured session
libra agent session stop <session-id>

# Resume a stopped captured session
libra agent session resume <session-id>

# List captured checkpoints
libra agent checkpoint list

# Page through checkpoints (default 50 per page; JSON carries next_cursor)
libra agent checkpoint list --limit 100
libra agent checkpoint list --cursor <next_cursor>

# Show a checkpoint's safe structural summary
libra agent checkpoint show <id>

# Replay a checkpoint as a JSONL transcript
libra agent checkpoint rewind <id>

# Drop temporary checkpoints from the most recent stopped session
libra agent clean

# Drop temporary checkpoints from every stopped session
libra agent clean --all

# Diagnose hook installation and capture state
libra agent doctor

# Push refs/libra/traces to the default remote
libra agent push

# Push refs/libra/traces to a named remote
libra agent push --remote origin

# Re-push after `libra agent clean` rewrote the traces chain (force-with-lease)
libra agent push --force-rewrite

# Discover libra-agent-<name> RPC binaries on PATH
libra agent rpc list

# Invoke a single JSON-RPC method on a libra-agent-<slug> binary
libra agent rpc invoke <slug> <method> --params '<json>'

# Run the DeepSeek Harness bridge over stdio (JSON-RPC 2.0 NDJSON);
# feed it a fixture and read one frame per response on stdout
libra agent bridge --stdio < bridge-initialize.ndjson

# Serve legacy MCP on stdio. stdout carries only JSON-RPC.
libra agent mcp --stdio

# Structured JSON envelope for agents
libra agent --json status
```

The same banner is rendered by `libra agent --help` so the doc and the
CLI surface stay in sync (cross-cutting `--help` EXAMPLES rollout, see
`docs/development/commands/_general.md` item B).

## Deferred parity (non-goals)

The following external-agent parity surfaces are intentionally **not** exposed
in this wave. They are recorded — with their handling and restart condition —
in the Agent tracing contract
([`../development/tracing/agent.md`](../development/tracing/agent.md), section
「还未实现的功能」), and are called out here so scripts and users do not depend
on them:

- **`agent add`/`remove` `--local-dev` / `--force` flags** are unpublished — use
  the canonical `enable` / `disable` (and their `add` / `remove` aliases) only.
  If they ever ship, each will be wired onto both the canonical verb and its
  alias.
- **Provider-specific transcript compaction / reassemble traits** are a future
  parity item. The writer already stores large transcripts as manifest-relative
  chunks, but there is no provider-specific compactor/reassembler yet.
- **Optional capability traits** (`ProtectedFilesProvider`, `TranscriptCompactor`,
  `HookResponseWriter`, `RestoredSessionPathResolver`, …) beyond the landed
  `DeclaredAgentCaps` matrix have no public behavior yet.
- **External-RPC method families beyond the v2 `info` / capability gate** are not
  implemented; an agent that does not declare a capability continues to fail
  closed.
- **The non-first-batch roster is unsupported.** Only `claude-code`, `codex` and
  `opencode` are supported, hook-installable and launchable for
  review/investigate. `gemini` (uninstall-only, see the Description above),
  `cursor`, `copilot` and `factory-ai` are `supported=false`, are omitted from
  `agent list`, and `add`/`enable` returns an actionable unsupported error;
  they are not launchable.

## Notes

- External `libra-agent-*` agents are **disabled by default**. Opt in with
  `libra config set agent.external_agents.enabled true` (repo-local); until
  then `rpc list`/`libra-agent-*` `rpc trust`/`rpc invoke` refuse with
  `LBR-AGENT-002` (`rpc untrust` stays available — revoking trust only
  tightens security; `rpc trust --dir <path>` and provider-exporter trust,
  e.g. `rpc trust opencode`, are preparation-only — they never scan `$PATH`
  and arm nothing by themselves, so they work while gated).
  Discovered binaries stay quarantined until `rpc trust <slug>` records
  their provenance (trust is refused for a binary in a world-writable
  directory), every invoke revalidates it (drift revokes trust,
  `LBR-AGENT-005`), the child environment is cleared to an allowlist, and
  stderr is captured/capped/redacted — never inherited. Invoke timeouts,
  broken pipes and malformed frames map to `LBR-AGENT-012`; IO hard-cap
  violations map to `LBR-AGENT-007`.


- Hook configs that `libra agent enable` installs for Claude Code and Codex
  invoke the top-level `libra hooks claude|codex <verb>` commands with an installer-owned bounded
  `--capture-budget-ms` argument derived from the handler timeout (default
  Claude `10s → 9000`, Codex ordinary events `30s → 29000`, and Codex
  `SessionEnd` `3s → 2000` because that host event has a three-second
  maximum; a one-second legacy handler keeps a usable `500ms` capture slice
  and up to 1000ms remains for terminal finalizer cleanup). Re-running enable refreshes older configs; the hidden
  alias accepts the same argument for legacy installed commands. The installed
  OpenCode plugin (`.opencode/plugin/libra-hooks.js`) instead forwards every
  event through the hidden `libra agent hooks opencode <verb>` entry, without an
  installer budget; its capture deadline is the OpenCode export deadline. Claude and the hidden alias reject a hook
  envelope that fails size / UTF-8 / JSON / schema / reported-working-directory
  / transcript-path validation with `LBR-AGENT-008` (exit 128), and never echo
  raw stdin. Both path-like fields are capped at 4096 bytes before storage
  resolution. The installed
  Codex surface intentionally acknowledges malformed/unbound callbacks and
  advisory nonterminal capture failures to avoid failing the host task. A
  well-formed callback invoked outside any Libra repository has no scope to
  bind: installed Codex hooks acknowledge it (exit `0`, including
  `SessionEnd`), while Claude and the hidden alias return the fixed,
  path-free repository-not-found error `LBR-REPO-001` (exit 128). A damaged
  active repository is not "outside" a repository: Claude, the hidden alias
  (Claude Code / OpenCode / Codex), the gemini entries and an installed Codex
  `SessionEnd` (which still exits non-zero) report the same
  path-free `LBR-REPO-003` (unresolvable linked-worktree storage, with the
  `libra worktree repair --confirm --path <worktree-path>` remedy), `LBR-REPO-002`
  (missing database or unsupported object format) or `LBR-IO-001`
  (unopenable database, including one written by a newer Libra, or
  unreadable object format) as every other repository command, never the
  generic `LBR-INTERNAL-001` capture failure (see `libra hooks`). Like every
  other repository command, these entries apply pending repository schema
  migrations when they open the database. A validated, scope-bound `SessionEnd`, however, must persist or observe a
  durable terminal completion/pending receipt; a bounded database/finalizer
  failure before that evidence exists is non-zero rather than silently
  acknowledged. After an existing catalog reservation, narrow nonterminal
  checkpoint or maintenance-lock failures may persist only a non-sensitive
  retryable catalog diagnostic. The budget bounds killable-helper, database,
  and other deadline-aware stages; it is not a strict whole-process host
  deadline because source classification, canonicalization, secure open, and
  rewind currently happen synchronously before descriptor handoff. A stalled
  NFS/FUSE operation therefore cannot be forcibly cancelled, and this path does
  not promise FD-only autonomous replay after timeout. A checkpoint operation
  (e.g. `checkpoint rewind`) on an inconsistent
  store — a catalog row whose `parent_commit` is malformed or points at a
  missing traces object — fails with `LBR-AGENT-009` (exit 128); run `libra
  agent doctor` to inspect the store.
- Managed Session Capture supports Unix and native Windows secure repository-private
  deduplication-key initialization. On other platforms it fails closed
  before creating key files, with an actionable diagnostic, rather than using
  a weaker path-based publication fallback. The fixed capability diagnostic is
  explicit for both Codex entry points: nonterminal `hooks codex` callbacks
  (including the hidden legacy alias) print a path-free platform remedy to
  stderr but exit `0`; `SessionEnd` returns non-zero with that same remedy.
  Unsupported non-Windows platforms use the Unix-host remedy; a native Windows
  initialization failure points to the matching `libra.exe` helper and local storage.
  The fail-closed installed Claude surface and the hidden `agent hooks`
  Claude Code / OpenCode alias return non-zero for every event with that same
  remedy and `LBR-UNSUPPORTED-001`. This exception does not change the alias's fail-closed malformed-envelope
  contract. It applies only to callbacks invoked inside a Libra repository:
  a callback outside any Libra repository follows the outside-repository
  contract above on every platform, Unix or not.
- `checkpoint rewind --apply` restores working-tree files only; the agent's own
  transcript file is not rewritten.
- Hook and capture diagnostics are best-effort and are designed to report
  actionable installation state rather than silently ignoring missing providers.

### Doctor checkpoint-store repair (`--repair`)

`libra agent doctor` scans the checkpoint store and writer-marker registry
(AG-20 repair matrix); without `--repair` it is strictly read-only
and reports what `--repair` would do:


OpenCode hook status now includes `provider_hooks[].events[]` in JSON. Each
entry has an `event` and the same nullable `installed` status as the managed
plugin: `true` means the exact current template is installed, `false` means it
is missing/outdated, and `null` means inspection was unavailable. Human output
lists these mappings as `(managed template)`. The native bus, merged inbox and
accepted compatibility mappings come from the existing event registry;
`session.error` is skipped. This describes installed mappings, not proof that
OpenCode ran or emitted each event. Reinstall an outdated managed template with
`libra agent add opencode` before starting a new session.

Before opening plugin files, doctor checks `.opencode`, both `plugin`/`plugins`
parents and both managed leaves with no-follow metadata checks. A symlink,
FIFO/socket/device, file parent or directory leaf produces a fixed slot/type
reason and unknown installation status. It does not open or modify that node,
print its contents or target, or mark it installed, including with `--repair`.
These checks retain the existing stable-directory-name assumption; they do not
resolve the deferred concurrent directory-relocation problem.

`checkpoint_store.opencode_legacy_reasoning_checkpoints` lists canonical IDs
whose bound local OpenCode metadata has no non-null `reasoning_state` field.
Human output marks them `legacy/unrecorded reasoning state; not evidence of
complete capture or native origin`. This is an absence diagnostic, not a date
or schema classification: current checkpoints can also lack that field, and
its presence alone proves neither valid reasoning nor native provenance.
Existing coverage completeness is not promoted or rewritten. The scan examines
at most 64 OpenCode catalog rows and 64 KiB of validated metadata per row
(4 MiB worst-case payload). Unavailable, oversized/unbound metadata or excess
rows produce a content-free incomplete-scan note; an empty list does not
certify complete capture. It reads no transcript or reasoning-artifact bodies.
The marker is read-time only, unchanged by `--repair`; removing this diagnostic
requires no data migration. These diagnostics first ship in the release
containing OG-06; the source change alone is not a release announcement.


| `inconsistency_type` | Meaning | `--repair` action |
|----------------------|---------|-------------------|
| `stale_catalog_row` | An `agent_checkpoint` row's `traces_commit`/`tree_oid`/`metadata_blob_oid` disagree with the checkpoint still reachable from `refs/libra/traces` | Rebuild the row's OID columns from the ref (idempotent UPDATE) |
| `missing_objects` | Checkpoint objects genuinely missing from the store (and the ref cannot rebuild them) — the best-effort object/manifest enumeration covers the E4 protocol tree: `manifest.json`, `events/lifecycle.jsonl`, `transcript/<agent_kind>.jsonl` incl. chunks, `redaction_report.json`, `content_hash.txt`, and the intermediate trees. It descends only the protocol-owned `events`/`transcript` subtrees and bounds tree entries, total objects, and manifest declarations; a cap hit is `manual_required`, never partial repair. It does not parse or verify lifecycle-line schemas, UUID derivations, or `identity_scheme` values. | None — reported `manual_required`; doctor never takes destructive action (try `libra fsck --heal` or a cloud/backup restore) |
| `missing_catalog_row` | A checkpoint reachable from `refs/libra/traces` has no catalog row (crash window B) | Re-INSERT the row via the writer's probe-first idempotent path, reconstructed from the commit's `metadata.json` (v1 and v2 shapes) |
| `missing_object_index` | Checkpoint objects missing from `object_index` (invisible to `libra cloud sync`) — covers the traces commit plus the bounded E4 protocol object set | Idempotent re-insert with the writer's row semantics (trees as `tree`, transcript blobs as `agent_transcript`, sidecars as `blob`). Before repairing a size, doctor streams and integrity-checks the held local loose object under the transcript cap; it never adopts manifest `byte_len` or renders the payload. |
| `expired_inflight_marker` | A valid traces writer marker outlived its TTL, including provisional preclaims and proven-created loose-object OIDs | Fence the expired writer in the final ref transaction, run the serialized repository-root proof, and retire the marker; inline recovery never unlinks shared payloads or removes `object_index` rows, leaving physical reclamation to repository GC |
| `invalid_inflight_marker` | Marker JSON, row identity, commit, or OID is malformed | None — reported `manual_required`; automatic deletion is unsafe because ownership cannot be decoded |
| `conflicted_coverage_claim` | Two different complete payloads claimed the same logical turn; doctor reports only a per-report opaque ordinal, bounded schema/revision/completeness facts, and that redacted challenger evidence was retained | None — reported `manual_required`; inspect the durable sanitized candidates and choose an explicit recovery rather than silently discarding provenance. `--repair` never chooses a winner |
| `inconsistent_subagent_content` | A current subagent content claim is missing or disagrees with its immutable revision, checkpoint catalog row, or association link | None — reported `manual_required`; unchanged replay fails closed until the companion relation and checkpoint objects/ref reachability are restored |
| `unresolved_subagent_link` | A current subagent content checkpoint has no unique provider-stable boundary match (the normal Claude case) | None — reported `manual_required`; content and boundary evidence stay independently preserved and doctor never guesses an association |

Additional rules:

- Repository and database failures keep the shared repository contract
  (exit 128). Outside a repository doctor fails with `LBR-REPO-001` and the
  same `libra init` / Git-conversion hints as other repository commands; a
  detached, migrating, or corrupt linked worktree fails with `LBR-REPO-003`
  and its verbatim worktree remedy. Doctor opens the repository database
  itself: a missing database is `LBR-REPO-002`, a database that cannot be
  opened (including one written by a newer Libra) is `LBR-IO-001`, and an
  unsupported `core.objectformat` is `LBR-REPO-002`. These database messages
  omit the database path and the stored object-format value.
- Pending terminal-finalizer diagnostics use read-only **keyset pagination**:
  32 sessions per page, at most 512 sessions and 128 receipt findings per run;
  receipt metadata is capped at 1 MiB before decoding. Malformed rows are
  reported through a content-free note while later valid rows remain visible.
  A skipped malformed key or reached limit means the report is incomplete,
  not that unlisted receipts are healthy or repaired.
- A receipt without a durable artifact is **`pending_source`** evidence;
  doctor cannot reopen provider sources or manufacture a replay snapshot.
  After a later stop/resume, superseded receipts remain `manual_required`,
  as do session-quarantined receipts. Even `--repair` leaves the current
  session and original receipt unchanged; quarantine is not successful capture.
  For an artifact-bound receipt that exhausts its original retry budget,
  quarantine routing moves only the private artifact header and preserves the
  original receipt revision, status, counters, generation and source fence.
  An eligible, authenticated artifact may be replayed locally only after its
  alias, receipt, marker and coverage fences are revalidated. One
  `doctor --repair` invocation counts at most five replay attempts. Automatic
  replays share one 2-second cooperative deadline, while each audited manual
  attempt gets its own fresh 2-second deadline; findings deferred by the cap
  or by a retry-later outcome are not counted, remain unrepaired, and
  rerunning `doctor --repair` continues the bounded queue.
  A finding is repaired only after the checkpoint write is durable and the
  original terminal receipt completes. An expired artifact gets one audited
  manual replay attempt after quarantine; failed or stale-fence attempts stay
  visible for manual recovery. A permanently refused automatic replay parks
  only the artifact header in quarantine; doctor does not retry that parked
  header or spend replay budget on it before the original retry/window limit
  permits an explicit audited repair.
- **Legacy-v1 checkpoints** (pre-AG-20 layout without `manifest.json`) are
  counted in `legacy_v1_checkpoints`, never enter checkpoint-object repair classes, and are
  never rewritten by `--repair`.
- Schema-v1 lifecycle JSONL lines have no typed `identity_scheme`; they remain
  opaque legacy evidence. `doctor`, `checkpoint show`, and `checkpoint export`
  do not infer HMAC/replay trust from those lines or from an event UUID.
- Checkpoints named by a **live traces in-flight marker** are writers
  mid-flight, not inconsistencies, and are skipped.
- Finding identifiers stay actionable without echoing damaged input:
  checkpoint-store findings carry the canonical checkpoint UUID (for the
  in-flight marker classes, the writer attempt's checkpoint id, which may
  never have been published), and `findings_store` findings carry the
  review/investigate `run_id` accepted by `libra review show` /
  `libra investigate show`. A stored checkpoint identifier that is not a
  canonical UUID is shown only as `checkpoint-id-redacted` or a per-report
  opaque label; `conflicted_coverage_claim` always uses its per-report ordinal.
- An **expired terminal writer marker** is not permission to bind a later
  transcript to its pending receipt. `--repair` retires only the stale marker
  ownership; it never changes the receipt's source or marker fence. Re-run
  the original hook/replay only while the same authorized source remains
  available. A same-native-terminal replay with a changed source is
  quarantined as `source_digest_conflict` for manual recovery; it publishes
  no checkpoint, traces ref, or stopped state.
- A **session without checkpoints is legal** (an active session before its
  first stop) and is never flagged; only checkpoint-without-session counts
  as an orphan.
- Captured **gemini rows stay readable** and are never flagged; leftover
  gemini hook *configuration* produces a hint pointing at the
  uninstall-only channel (`libra agent remove gemini`).
- A catalog-row repair for a **scoped** captured session revalidates that
  session's durable workspace lease as the final database write. If the
  lease was released, expired, or re-fenced, doctor leaves the finding
  unrepaired and `manual_required`; only explicit `legacy_unknown` rows use
  the historical unfenced compatibility repair path.
- All repairs are idempotent — running `doctor --repair` twice performs no
  work the second time. With `--repair`, one `agent.doctor.repair` tracing
  span is emitted per repair attempt (`inconsistency_type`, `repaired`,
  `manual_required`); transcript content never reaches the log.

## Reasoning type contract (RG-01)

The RG-01 reasoning type contract is currently metadata-only for durable
capture; it does not change the capture/export behavior above. Its five states are
`provider_visible`, `encrypted_unavailable`, `opaque_archived`, `not_present`,
and `unsupported_shape`. `encrypted_unavailable` means a provider-declared
ciphertext has no authorized decryptor, not that capture failed. Unknown
reasoning-like shapes require a payload-free warning and a partial turn when
provider wiring is added. A text/tool block selected for encrypted-field
verification is rejected as `unsupported_shape`, even with an undeclared
signature-like key; ordinary content is classified before that verifier.
`OpaqueEncryptedBytes` cannot be serialized into an
ordinary transcript or converted into `RedactedBytes`. Only the reasoning
module can construct it, after verifying the shape and JSON-string type of a
Claude assistant `thinking.signature` or `redacted_thinking.data` field from
an already authorized source; its original escaped UTF-8 bytes are kept without
JSON re-serialization or base64 decoding. A syntactically valid record is not
automatically trusted — a matching JSON shape is not
proof of provider identity.
OpenCode 2.0.24 `reasoning.state` is open-ended and cannot establish a verified encrypted source.
No live provider adapter yet produces artifacts from this type contract.

This type-contract verifier accepts only the minimal synthetic envelope; real session records and additional metadata are deferred to the reviewed provider adapter contract.

## Readable reasoning projection (RG-04)

RG-04 adds the readable-reasoning projection path:
`ProviderVisible` reasoning text is the only reasoning content that can enter
the coverage projection, and it always passes typed redaction first — canary
secrets inside reasoning text are counted in the redaction report and removed
before projection, checkpoint persistence or any metadata/digest write. Readable
reasoning projects under its own `reasoning` record type with `provider` and
`source_kind` metadata; it never impersonates an assistant answer, and it is
never written to unredacted metadata or logs.

Direct serde serialization/deserialization of `ProviderVisibleText` is deliberately rejected. Callers must use classification and typed redaction followed by `canonical_turn_bytes` or `safe_turn_projection`. RG-04 does not wire a live provider adapter.


RG-02 archives verified ciphertext as opaque artifacts: each checkpoint
manifest may carry a `reasoning_artifacts[]` array (path/oid/sha256/byte_len/locator/
provider/source_kind/availability/decrypt_capability) storing byte-exact
objects under `reasoning/encrypted/<sha256>`; duplicate ciphertext bytes are
deduplicated to one object, duplicate locators are rejected, and fan-out is
bounded fail-closed (512 entries / 256 KiB manifest / 32 MiB total). The
`content_hash` still covers only the four plain roles; an empty artifact set
leaves the checkpoint tree byte-identical.

New opaque artifact objects use standard zlib stored blocks (level 0) to bound
compression CPU. Their canonical blob bytes and Git OID stay unchanged. This
uses more storage and mirror bandwidth for high-entropy ciphertext; ordinary
objects retain default compression, and valid existing objects are reused
without recompression.

### Controlled opaque reasoning export

Libra preserves provider-encrypted bytes; it neither owns nor guesses decryption keys. Readable reasoning text passes typed redaction; a provider signature authenticates provider output, redactedData represents a provider-redacted reasoning block, and reasoningEncryptedContent is opaque provider ciphertext. None of these metadata labels alone proves native origin. The existing native source requirement remains in force.

For OpenCode exports, Libra now classifies assistant reasoning before the
ordinary transcript Redactor. Visible reasoning text remains in the ordinary
transcript and is redacted. Reasoning-bearing turns remain Incomplete under
coverage v1; they do not gain a new digest record. The ordinary copy excludes
`messages[].providerContext.messages` and assistant reasoning metadata beyond its
visible `id`, `type`, and `text`.
Provider metadata on parts of every role, message envelopes, and the export
document/session info is also removed before redaction, except that completed
tool `state.metadata.files[].file`
strings retain a minimal modified-file projection. Other tool metadata fields
are omitted. The producer-owned Boolean `state.metadata.truncated` is recognized
and dropped without changing completeness, including `false` on routine
successful tools. In a completed tool, a wrong-typed `truncated`, another
metadata sibling such as the `outputPath` written for truncated output, an
unknown `files[]` child, or a wrong type makes capture partial; `files[].diff`
is omitted without changing completeness. Running/error tool metadata is
omitted without changing completeness unless it carries a named opaque hint.
Such a partial export is ineligible for live checkpoint writes
(`SourceRejected`) and verified imports (`ImportError::SourceAuthorization`);
partial is not merely a warning.
Native completed/error tool
`state.content` keeps clean visible text after nested metadata scrubbing; a
named opaque hint makes it partial. Classic tool `state.content` is unsupported
and omitted with partial status. Known classic
nonreasoning part identifiers, step fields, timestamps and safe tool
title/time/file attachments remain. Native model variants, compaction models
and safe error/retry headers remain, with schema-shaped native/classic fixtures
required to retain ordinary coverage and extractor projections. Provider
response bodies/headers, classic format/summary/structured payloads and
session fork, permission, revert or share metadata are omitted with partial
status even when those fields are valid upstream.
This includes classic user `info.summary` objects and assistant summary flags.
Nonempty native user `files`, native `skills[].text`, and additional native
location workspace identifiers are also omitted with partial status.
Retained message and part envelopes use closed field and type projections:
unknown child fields or wrong-type values make capture partial, and nested
provider metadata and named opaque hints cannot ride in user attachments,
classic `info.model`, assistant `model`, errors, or other envelope objects.
An unsafe non-assistant part is omitted as a unit; an independent safe prompt
part remains. Native tool `state.input` and classic tool
`state.input` are free-form ordinary user/tool content. Classic `state.output`
is retained as ordinary content only when it is a String; an object or array
is removed with partial status. Same-named JSON keys in free-form content
alone never prove native origin. Native
`state.output`, `state.raw`, `state.title`, `state.attachments` and
`state.time` are outside the 2.0.26 native schema and are removed with partial
status.
A candidate `state.signature`, `state.redactedData`, or
`state.reasoningEncryptedContent`, an unknown state shape, or a malformed proof
keeps capture partial with no artifact until native origin is verified. Its
reasoning `text` is also omitted from the ordinary copy because it may repeat
or contain fragments of the opaque value. Malformed
or unsupported export envelopes yield a content-free partial snapshot with the
existing generic `SourceAuthorizationMismatch` reason. A
same-named SDK metadata field on reasoning also stays partial and is removed
from the ordinary copy. Installed
OpenCode/model acceptance remains a local macOS check, outside CI.
The capture preprocessor accepts one JSON export object with a `messages`
array. Flat message records, JSONL, missing or wrong-typed `messages`,
duplicate keys, and malformed JSON are content-free partial captures with
`SourceAuthorizationMismatch`; they cannot become ordinary checkpoints.
The standalone OpenCode metadata extractor retains its best-effort flat/JSONL
compatibility, but it does not bypass this capture gate. A parseable export
with an unknown root, message, or classic-info key drops that key and remains
partial while preserving independently safe visible text. The fixed partial
reason does not print the rejected payload; use a supported export to retry.

`checkpoint show` keeps its fixed human-readable safe summary. `checkpoint show --json` adds `reasoning_artifacts[]` only when a checkpoint declares artifacts. Each item exposes only `provider`, `source_kind`, `locator`, `byte_len`, `sha256`, `availability`, and `decrypt_capability`; it never exposes artifact paths, object ids, arbitrary metadata or bytes. Default show, transcript export, scoped input and skill readers do not read artifact bodies. An invalid checkpoint layout or manifest fails closed without reflecting its contents. A valid catalog root whose object is not yet available keeps the legacy safe summary, omits the optional artifact array and emits a fixed content-free diagnostic; present corrupt roots still refuse. This compatibility fallback never authorizes export.

```sh
libra agent checkpoint show <checkpoint-id> --json
libra agent checkpoint export <checkpoint-id> \
  --reasoning-artifact <lowercase-sha256> \
  --allow-raw --justification "operator recovery" -o <new-private-path>
```

Opaque export requires all three authorization/output options, a lowercase SHA-256 and a new destination. Do not combine it with `--raw`. Authorization is checked and an independent `reasoning_artifact_export` audit row is appended before checkpoint lookup, artifact body access or file creation. A grant records the authorized attempt; later lookup or file failure retains that audit row. Audit failure stops the operation before body access.

The selected artifact must belong to the selected checkpoint. Export verifies the closed manifest/tree role, complete blob object identity/type, SHA-256 and any declared byte length. The maximum is 32 MiB; truncated prefixes are refused. Bytes bypass redaction and are never decoded, sent to stdout, used as model input or decrypted. macOS/Unix writes a 0600 temporary file beside the destination and publishes it without overwriting an existing file, symlink or directory. Missing parents are not created; failed writes remove the private temporary file. Platforms without this owner-only permission primitive refuse with `LBR-AGENT-013` and a denial audit. Windows private ACL export is deferred; default metadata and ordinary transcript readers remain available.

The `ReasoningDecryptor` registry returns `None` for Codex, Claude Code and OpenCode. `decrypt_capability` stays `none` and encrypted bytes remain `encrypted_unavailable`; a future official, explicitly authorized provider capability must enforce checkpoint/provider scope. This release implements no decryptor and accesses no decryption keys.

Artifact tree reachability is added by RG-06; controlled read and export
surfaces are added by RG-03.


Checkpoint readers verify the object type, content-addressed identity and ordinary-role bindings before consuming a manifest or metadata body. Skill search skips metadata that does not belong to the catalog's named checkpoint. The fresh `--checkpoint` resolver uses the explicit repository catalog in read-only mode and refuses missing or inconsistent stores without creating a run. Saved-input resume revalidates the persisted spec against the same named catalog and manifest/tree role proof before creating a new input attempt. Default checkpoint show/list summaries and raw-export authorization/audit remain unchanged. Each skill candidate is checked independently: ordinary child trees are capped at 16 MiB and the selected metadata blob at 16 MiB; the checkpoint binding is checked before walking the ordinary closure.

For legacy OpenCode checkpoints, ordinary `checkpoint export` and the bound
metadata reader remove `metadata.anthropic.signature`,
`metadata.openai.reasoningEncryptedContent` and legacy `encrypted_content`
from reasoning parts and reserved export/checkpoint metadata. Readable reasoning,
other metadata and arbitrary tool input or user JSON remain intact. Provider
classification uses the checkpoint's session catalog; a present metadata kind
must agree. Missing session bindings, inconsistent metadata, duplicate keys,
malformed or unsupported OpenCode JSON, and truncated nonempty exports are
refused with a payload-free diagnostic. Empty transcripts remain valid.
`--raw --allow-raw` keeps the original bounded bytes and the existing audit;
`checkpoint show` remains a fixed safe summary. Stored objects are not rewritten,
and future fixes must retain this ordinary-output protection. This legacy
reader does not establish native provenance or implement the newer `state.*`
reasoning artifact channel.

The macOS local content gate is `local_capture_smoke_opencode_macos` in
`agent_local_capture_smoke_test`. It requires an explicit local-agent opt-in,
pins the private OpenCode `2.0.26-libra-native.1` via
`LIBRA_OPENCODE_NATIVE_BINARY`, selects `volcengine-agent-plan/glm-5.3-flash`, and checks
the disposable project before requesting a single fixed file write. The gate
checks captured prompt/model/tokens/files against the source projection,
persisted file metadata, stopped sessions, doctor and uninstall retention.
A failed or timed-out preflight is a failure, with no model request started.
At the historical NPR-18 acceptance checkpoint, OpenCode's private server did not report
readiness; real content acceptance is deferred as `DEFER-OG-OPENCODE-BOOT-01`.
The harness and synthetic tests do not prove real content or native reasoning.


### Private native-origin issuer registration (macOS)

The installation challenge runs with fresh, owner-only home, configuration, data, cache, state and temporary directories inside the owned private installation. Its initial environment is constructed explicitly; caller `TMPDIR`, runtime-loading overrides and host HOME/XDG settings are not inherited. The temporary directory must be canonical, owned by the current user, mode 0700 and empty before the producer starts. A failed precondition or cleanup refuses registration with the existing fixed challenge error.

Scratch is retained for the child lifetime and cleaned after normal exit, failure or timeout; cancellation cleanup is best effort. Inherited process-group descendants are terminated while the leader PID is still reserved. A parent crash or SIGKILL can leave an owner-only scratch directory, which later challenges never reuse. This isolation applies to the Libra-launched installation challenge. Separately launched OpenCode sessions retain their own runtime requirements, and the current `.3` SOURCE/Source A and real-field archive gates remain unproven.

Ordinary `agent rpc trust opencode` pins an exporter binary; it does not register an issuer or prove native reasoning. The historical native path requires a separately installed, reviewed macOS arm64 `2.0.26-libra-native.1` or `.2` producer and its exact frozen v1 observer recipe, rendered for the current canonical Libra executable path. Prepare that observer using the reviewed private bundle's instructions in an isolated owned setup. Global OpenCode remains independent.

The immutable catalog also pins a separate `2.0.26-libra-native.2` candidate (build `78c1f19e9249da8d0143a079f90aec9a`, macOS arm64 binary SHA-256 `28174e893a2b63b75f58af4c77212747f6e1eebce5dd0252cfe64847b342615c`). Its additional endpoint descriptor v2 permits only the first-party `anthropic-messages` route to the exact URL `https://api.anthropic.com/v1/messages?beta=true`; the receipt and Ed25519 attestation remain v1. Anthropic-compatible providers, proxies and changed URLs do not match this endpoint. Selecting `.2` still requires separately measured binary bytes, a fresh private installation/key, explicit exporter trust and explicit epoch-checked issuer registration. The `.1` issuer can remain historical verify-only; no key is automatically promoted. The `.1` build remains the frozen NPR-18 harness baseline, not the current official29 ordinary baseline. A catalog match or synthetic test does not prove real `signature`/`redactedData` source bytes or allow their archival; `reasoningEncryptedContent` remains deferred. Provider/account/model checks stay local manual gates, outside CI.

The legacy v1 observer is distinct from the current ordinary v2 observer. It lacks per-directory filtering: never use it with a shared multi-project OpenCode server. Prefer current ordinary capture; any legacy use must isolate one repository per server. `agent doctor`/observer status reports this old v1 plugin as not installed because current detection requires the v2 header. `libra agent enable --agent opencode` replaces a managed v1 plugin with v2 and breaks the old private recipe match. Generic reinstall or `agent enable` is therefore not a repair for legacy issuer registration. This compatibility fix does not change the ordinary per-directory observer or add a legacy installer.

The following registration example assumes the exact reviewed historical observer is already in place; it deliberately omits `agent enable`. The published-binary compatibility gate uses a fresh owned repository, HOME, XDG directories and epoch-0 registry with the original `.2` public descriptor and issuer/key unchanged. Only the measured producer's registration challenge may access that key. It runs no Session, account/model request or OpenCode server, and revokes the fresh registry during cleanup. A successful gate proves registration compatibility only, not Source A, a current `.3` producer or real native-field archival.

```sh
libra agent rpc trust --dir /absolute/approved-private/bin
libra agent rpc trust opencode
libra agent rpc trust opencode \
  --native-origin-manifest /absolute/private-installation/installation.json \
  --native-origin-expected-epoch 0
```

The two native options must be supplied together and only with the literal `opencode` slug. Initial registration expects epoch 0; later registration requires the current repository registry epoch. Inspect `libra config --get agent.native_origin.opencode.v1` locally before an explicit retry. A conflict refuses without changing trust or registry. The registry includes local installation paths and should remain local.

The public descriptor must be a regular, non-symlink `installation.json`, owned by the current user with mode 0600 in a canonical owned 0700 directory, at most 64 KiB and in the private producer's closed minimal JSON format. Libra does not read the private key. Existing exporter trust, actual approved producer bytes, the current canonical Libra executable path and exact installed `.opencode/plugin/libra-hooks.js` bytes must match. A descriptor or export cannot authorize its own path, public key, endpoint or build.

The actual producer must successfully answer a new random, domain-separated Ed25519 challenge within 30 seconds, with at most 4096 stdout bytes and bounded captured stderr. Child output is withheld on failure. After the challenge, one write transaction rechecks current exporter trust and trusted directories, performs expected-epoch CAS, and writes one complete registry row. Duplicate rows, case aliases, encryption, malformed records and size overflow refuse without repair. Limits are 64 KiB and 128 issuer records; revoked history is never removed to make space.

A new independent installation key becomes active; earlier active keys become verify-only. Existing issuer keys cannot be reactivated by registration or ordinary binary re-trust. Historical records remain immutable. Successful registration proves installation key/binary association only. Receipt signature, current issuer permission, source coordinates and original native field checks remain required before any archive admission; unknown or unproven sessions stay partial. JSON registration output includes the transient current Libra executable SHA256 for local audit; this measurement is neither a signed policy field nor a pinned historical Libra version.

An unapproved row manually created against the unreleased development `cd53…` observer policy remains rejected. Preserve and back up the entire registry and repository, and suspend affected native registration/archive operations pending a separate reviewed repair. Do not delete rows, automatically migrate them or treat them as approved. The bounded compatibility checks do not establish that every user repository has been scanned; supported ordinary capture can be used in a fresh isolated workspace without claiming the affected repository is repaired.



### OpenCode native-origin rotation and revocation

To rotate an installation key, prepare a new independent private installation and explicitly register it with the current epoch. The earlier active issuer becomes verify-only. Verify-only and revoked issuers cannot authorize new native capture, import or pending replay. The current permission reader requires one transaction snapshot, fresh independently measured producer provenance, matching exporter trust and a trusted directory; it returns installation data, not a verified receipt or native artifact.

`libra agent rpc untrust opencode` now removes OpenCode exporter trust and revokes every remaining active or verify-only issuer in one bounded write transaction. It preserves all records and already stored artifacts. It remains available while external RPC is disabled. Malformed or duplicate registry rows, capacity/epoch failures and write faults refuse without a partial trust or registry update. Check and preserve the local registry before retrying; it is never automatically repaired. A repeated successful untrust is idempotent and does not advance an already revoked registry epoch.

JSON retains `slug` and `removed` (`removed` means an exporter trust row was removed). OpenCode additionally returns `native_revoked` (the number newly revoked) and `native_epoch` (the resulting epoch, or null when no registry exists). Other slugs retain their existing output and behavior.

If combined untrust refuses because the registry is damaged, suspend exporter access with `libra config unset agent.trust.opencode`. This removes binary trust only and preserves the registry, including all tombstones. A still-active issuer can regain permission after the same approved binary is explicitly retrusted. Repair the registry while preserving its records, then successfully run combined `libra agent rpc untrust opencode` for permanent native revocation.

Automatic provenance revalidation after an unreadable or changed binary or withdrawal of its trusted directory still removes only exporter trust. Native admission pauses while that trust is absent or mismatched; it leaves the issuer registry unchanged. Explicitly trusting the same reviewed build again can restore an active issuer that was never explicitly revoked. Ordinary binary trust never restores a verify-only or revoked issuer. Re-registering an old issuer is refused: rotation requires a fresh independent key. Registration/revocation do not rewrite historical receipts, private installation descriptors or immutable artifacts. Successful lifecycle operations do not prove native field origin; unproven content stays partial.


### Frozen historical private OpenCode acceptance baseline (NPR-18)

The frozen NPR-18 macOS arm64 harness uniquely accepts `2.0.26-libra-native.1`, build
`bac21cc4027896b8d1836133c347d20f`, SHA-256
`547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196`
and 187635570 bytes. It is based on official OpenCode `2.0.26` commit
`9b4ec5714d481559990db0a816d5dec19541a814`. That commit identifies the official
base; the separate source, patch, lock and build-input digests identify the
patched build. The public `native-origin-build.json` descriptor contains those
reviewed identities and no installation policy, issuer or private key.

Set `LIBRA_OPENCODE_NATIVE_BINARY` to the absolute path of that independently
installed private `opencode` before macOS local capture acceptance. The harness
requires the reviewed version, build digest and size before login or any model
request, bounds the version and login probes to 30 seconds, and puts the same
private binary directory first in each capture/export child's PATH. Missing
opt-in or drift fails the gate; it never falls back to global OpenCode.
The official global installation and existing observer ABI remain separate.

NPR-12's SOURCE_READY_GO covers this reviewed darwin-arm64 build and the current
OpenAI Responses transport mechanism. The existing official source record,
event/export fixtures and synthetic receipt/signature vectors remain historical
and unchanged. Source A is UNPROVEN: deterministic tests, a version string,
registration and this descriptor do not prove real native provider fields or
permit reasoning archival. Failed readiness or quota gates stay partial; the
only authorized paid smoke model is `volcengine-agent-plan/glm-5.3-flash`, with
no alternate-model fallback. This card makes no new model calls. Linux local
functional acceptance remains with the operator.


### Provider-encrypted reasoning lifecycle

`libra cloud sync` validates the reasoning artifact manifest role, canonical tree, `object_index`, decoded `byte_len`, and full SHA-256 before beginning a capture generation. A missing first upload is repaired normally; post-upload readback failure prevents completion. The validated checkpoint trees and artifact indexes must match the final fenced generation. No ciphertext is rendered by this validation.

Session erasure removes artifact references from checkpoint list/show and the session graph; `agent clean --gc` and repository GC reclaim unreachable local artifacts under the existing retention windows. `agent doctor` reports `inconsistent_reasoning_artifact` with the exact canonical `checkpoint_id`; this read-only finding requires manual repair and does not create a persistent checkpoint status column. Physical R2 payload deletion remains deferred; cloud tombstones remove mirror catalog rows and prevent resurrection through normal restore.

Non-quiet `agent enable` and `agent import --yes` disclose that provider-encrypted reasoning original bytes may be archived without decryption and mirrored to cloud storage with their checkpoint. Synthetic lifecycle fixtures remain `encrypted_unavailable` while visible and disappear after erasure; they prove neither native provenance nor a decryptor. Native source proof remains required; unproved OpenCode fields are not archived.


### Authorized reasoning source carrier (OG-16)

The selected registered adapter has an optional pre-redaction source preprocessor, disabled by default. It separates only an already authorized, bounded in-memory source; it cannot reopen provider files or access the checkpoint store. With no capability or no candidate, ordinary snapshot, live and store bytes retain their existing behavior. When enabled, candidates are removed before the ordinary Redactor. Unproved or unmapped candidates retain a payload-free partial classification, including a partial with zero artifacts; they cannot be converted to ordinary complete content.

A sealed native carrier moves original opaque bytes and their source-bound proof together into the shared checkpoint payload. Native issuer, provider, source session, message, part, field, wire SHA-256 and authorized capture/turn association are validated before object or index writes. The metadata-only identity adapter cannot expose payload bytes or mint native authority. Debug output does not contain native coordinates or payload. Source, artifact-byte and artifact-count limits remain enforced.

The authenticated private import transport carries a native-proven typed collection from the authorized descriptor helper to the parent. A one-request key binds the helper response to the request, authorized source, repository scope and collection; the parent verifies that binding and retains the complete native proof before assigning owned items to reserved turns and the shared checkpoint store. An unauthenticated or unproved nonempty collection fails closed, without treating a wire DTO as native authority or returning candidate bytes to the ordinary transcript. Empty preparation keeps its existing response and source projection bytes. Pending v1 still refuses nonordinary payloads before writing an envelope; pending v2 accepts only a validated borrowed native carrier and its prepared identity plan under the existing receipt, fence and lease rules. For an enabled Claude deadline-FD route, preparation is rejected before source resolution, without an ordinary fallback. An existing Pending receipt prevents the wrapper failure diagnostic; NotTracked retains only its existing bounded diagnostic attempt and does not advance sync_revision.

Production provider capabilities remain disabled and no production native issuer is admitted. Synthetic positive fixtures prove the carrier and import transport mechanism only. Source A remains UNPROVEN and real native field admission is deferred; unproved OpenCode fields are not archived. Durable reasoning replay still requires its later gate. This mechanism adds no decryption capability or public metadata enum value.

### OpenCode sealed candidate before native admission (OG-26)

For an authorized OpenCode export, capture checks JSON structure before building its parse tree, then privately separates a closed assistant reasoning `state` and the original `originProof` bytes before ordinary redaction. The private candidate retains decoded UTF-8 values and source session, message, part and field coordinates; it is bounded to the 16 MiB source cap, 65,536 JSON nodes, 64 nesting levels, 8 KiB per receipt and the existing 512-artifact count. The ordinary copy excludes these private values, and an unverified candidate remains a partial snapshot with no ordinary checkpoint consumer. Malformed or over-budget input yields a content-free partial result; structural budget failures report `source_oversize`, rather than an authorization mismatch. A long session can exceed the 65,536-node limit even below 16 MiB; start a smaller capture session and inspect the content-free partial reason before retrying. On rejected-source and sealed-candidate paths, owned raw source, candidate, and parsed-tree allocations are wiped, but Serde's internal scratch and partially constructed values during both successful and failed parsing cannot be individually wiped. This candidate cannot issue native proof or write a reasoning artifact; issuer verification, real field admission and replay remain separate gates. OpenCode CLI/hook/export-to-Session capture integration checks, including deterministic fake-exporter acceptance, are explicitly ignored in CI and run only as macOS local acceptance. Pure parser and sandbox unit tests remain deterministic CI checks. Installed OpenCode and model-response checks are also local macOS acceptance only.


### Pending reasoning v2 codec (OG-17)

The private pending codec pairs its closed inner payload v2 with the live SessionEnd writer and authenticated reader. When an admitted producer supplies a bound native-proven collection, the writer seals its source record and explicit per-turn `verified_nonempty`/`verified_empty` identities. Ordinary pending payloads remain byte-for-byte v1; the outer header and envelope remain v1 with the existing MAC domain, 64 MiB envelope, 128 chunks and 512 KiB-per-chunk limits. A pre-v2 reader rejects v2 and retains its evidence. The current reader checks the original whole-envelope MAC, canonical member/claim association and current catalog, claim and marker fences before the lower reasoning issuer validates the fixed source proof. It restores only reserved-turn artifacts and commits their original stable identities with the checkpoint, claim and ref transaction. A failed append retains the pending artifact and claims; each retry reauthenticates the original bytes and current fences without reopening a provider source or minting a replacement key. Missing or damaged private-key/source evidence requires diagnosis and manual recovery. A decoded DTO or envelope MAC alone never proves native provider origin: unproved fields remain partial and are not archived. Production native issuers and real OpenCode Source A evidence remain disabled; synthetic tests exercise this transport without claiming real native archival.

### Private reasoning identity and garbage collection

Garbage collection excludes the reserved `agent_reasoning_identity` scope from generic metadata object-reference scanning. This applies to the entire stored value, including a damaged value containing a JSON object ID or a nested object ID; those values do not trigger object hydration. This exclusion requires no private reasoning key. Ordinary metadata object roots, private capture recovery exclusions, and capture pending parent roots retain their existing rules. Forward recovery must retain both the private rows and this exclusion while a reasoning identity writer is stopped or repaired. This guard does not create identity records, issue native provenance, or enable OpenCode reasoning archival.

### Reasoning identity coverage and session limits

When reasoning coverage cannot be safely compared, live capture and export fail closed with `reasoning coverage is partial; run libra agent doctor before retrying capture`. A complete-turn identity conflict is reported by doctor with the reason `reasoning_identity_changed`; it requires manual review of the authorized reasoning artifacts, and doctor does not choose a winner.

The private identity namespace has separate `identity-v1` and `identity-v2` keys. Together they are limited to 4096 records and 1 MiB of stored encoded values per session. Reaching either limit stops the write and reports `private reasoning identity session map reached its safe capacity; start a new capture session`. An absent map is the legacy state: candidate-free ordinary turns retain their existing coverage behavior, and the first eligible identity-bearing turn may create a map. A malformed, oversized, unexpected, or unauthenticated map rejects the entire reservation with `private reasoning identity is invalid; start a new capture session` before any claim write, including candidate-free turns; an import receipt records this as failed. Doctor does not repair an invalid identity map, and a missing conflict finding does not make the session complete. Do not edit the private metadata scope manually.

These checks compare capture identity only; they do not prove native provider origin. Unproved OpenCode reasoning remains partial and is not archived.


### OpenCode native import directory binding

Native OpenCode exports bind the session directory in `info.location.directory`.
Import also accepts classic `info.directory` and `info.cwd` declarations. It
collects all declared string directories and requires them to resolve to one
canonical directory in the current Libra storage boundary; no field takes
precedence over a conflicting declaration. Directory fact collection does not
coerce non-string values; the existing source and schema validation still applies.
Equivalent lexical paths and symlink aliases keep the same canonical scope
checks. A missing or conflicting directory returns `LBR-AGENT-016`; a directory
belonging to another repository stays rejected. This compatibility fix does not
approve native reasoning origin.


### OpenCode 2.0.29 idle error projection

The public 2.0.29 idle-message schema adds an optional `error`. An absent error
keeps the ordinary projection. A present diagnostic header requires string
`type` and `message`; optional HTTP `status` is an integer from 100 to 599.
Missing required fields, a wrong shape or an invalid status removes the whole
error and marks capture partial. A `response` body and unknown members are
removed and keep capture partial; an opaque hint removes the entire error.
Existing compaction and retry error handling is unchanged.

These are ordinary typed diagnostics, not native producer or field proof. The
new regression fixtures are synthetic. Actual idle-error output from a live
model Session remains unproven; a successful ordinary session does not prove
that failure path. Real OpenCode/account/model checks run locally on macOS
and stay outside CI. Native reasoning Source A remains a separate gate.


### OpenCode ordinary partial live checkpoints

Automatic live capture can save a safe ordinary copy of an authorized OpenCode
export when unsupported reasoning was discarded. The checkpoint remains
`Unproven` / `Partial`; saved content is not complete reasoning coverage or native
provider evidence. At least one known non-reasoning text or message part with
usable content must remain. A source containing only metadata or erased reasoning
skeletons is refused. This route accepts the current native `messages` export;
legacy classic `info` / `parts` exports remain excluded.

This storage-only route requires strict single-JSON parsing, successful exporter
authorization, exact session mapping, bounded redaction and repository-scoped
source HMAC binding. The source session's `info.id` must equal the selected
provider session. The owning carrier records `repo_id`, Libra session, provider
session and agent kind at bind time and checks them against the actual checkpoint
request. The HMAC is an opaque string; its format alone does not verify the scope
or prove native origin. Generic ordinary consumers stay closed to `Unproven`.

Unsupported reasoning state, including `reasoningField`, is written with the readable
associated reasoning text and time. The ordinary transcript keeps that text, `time`,
`state`, and the other unproven fields. Opaque hints include `signature`,
`redactedData`, `reasoningEncryptedContent`, `thoughtSignature` and `originProof`.
An original `originProof` key anywhere in the parsed source, including malformed
or unfamiliar nested values, or a sealed candidate prevents this fallback.
Only the completed scrub and redaction result can enter storage; a metadata flag
cannot grant this capability.

For this verified storage-only route, `source_authorization_mismatch` also means
reasoning or unrecognized data was discarded; exporter authorization has already
passed. Other paths retain their existing rejection behavior. The marker still
means `Partial`, and Source A remains UNPROVEN. No reasoning artifacts, private
reasoning identity, Complete coverage claims or native ACK are created.

The source input cap bounds each snapshot independently (up to 16 MiB).
Different idle actions can duplicate whole-source snapshots in content-addressed
storage. Cumulative retained snapshots have no per-session total cap and can grow
with idle count × source size, before redaction and storage overhead. This route adds
no cross-idle deduplication protocol; existing action, generation, lease, scoped
tombstone and ref-CAS fences remain in force. A successful ordinary lifecycle
receipt follows a verified durable partial checkpoint write. A failed direct
write stays dirty or leaves a content-free pending intent, without a successful
ACK or generation advance. There is no durable partial payload replay.

Explicit import of this unsupported-state source remains refused with
`LBR-AGENT-020`; this live fix does not enable import or native admission. The
stored ordinary payload remains a single redacted JSON document, with no new
public reason or metadata enum value. Checkpoint/export readers in this build use
that ordinary payload. Older-binary compatibility requires a separate actual
readback and is not inferred from the source shape; this does not relax the V2 import rule requiring partial
projections to have zero bytes and zero redaction metrics.

The official 0.30.49 release includes this ordinary live fix and can save its safe
ordinary copy under the closed rules above. Release completion does not prove native source
or complete later COV family gates.
For historical OpenCode imports, sessions containing unsupported reasoning state
remain rejected with `LBR-AGENT-020`; their repair is deferred. Real
OpenCode Session/account/model checks run only locally on macOS and stay outside
CI; synthetic tests do not prove native source.


### Current official OpenCode ordinary source baseline

The current official ordinary source reference is OpenCode **2.0.29** at
`042d0832f5f3a73eaa039a58a7d565776f6d4290`. It uses an explicitly measured
official binary and a server/exporter of that same version for local acceptance.
The frozen 2.0.26 source record, existing JSON fixtures and separate private
`.1` / `.2` identities retain their original meanings. The old private `.1`
acceptance helper still accepts only its exact version, binary and build ID.

Official version/source matching does not approve a native producer, register
an issuer or prove any opaque field. Ordinary exporter trust is never native
producer approval or Source A. The separate native.3 registration and real
field-source gates remain independent and do not block ordinary capture;
151 mechanical producer cases are not
whole-producer source/full acceptance.

The 2.0.29 idle header requires string type/message and optional integer HTTP
status from 100 to 599; malformed or sensitive error members stay partial.
Offline synthetic tests cover that adapter; real idle-error failure output
remains unproven.

This ordinary baseline is scoped to automatic capture, a durable checkpoint and the
ordinary redacted export of that checkpoint, with prompt, model and token
projections compared to the authorized source. Unsupported reasoning state is
retained with its text. The source/snapshot status pair is `Unproven` / `Partial`: `Unproven`
is the internal source classification, while `Partial` is the persisted
completeness value. Readable reasoning text is retained. Associated reasoning time is retained with the unproven fields. The model comparison uses `extraction.model` from the
native assistant record. The top-level `model` is hook metadata and can differ.
Token totals use distinct native assistant records without counting the session
summary twice. A no-tool sample proves only empty-files consistency, not a
write/edit/apply_patch positive.

For historical OpenCode imports, sessions containing unsupported reasoning state
remain rejected with `LBR-AGENT-020`; their repair is deferred. This automatic
capture baseline does not enable historical import, pending replay or native
admission. Ordinary exporter trust is never native producer approval or Source A.
Source A remains UNPROVEN.

Reuse earlier automatic-capture evidence only after checking the final source
and configuration and verifying readback with the actual new binary. Unverified
checks remain pending. A successful ordinary sample does not establish native
fields, complete reasoning, real idle.error failures, automatic terminal
completion or full-suite acceptance. Real OpenCode Session/account/model checks
run locally on macOS and stay outside CI. Metadata-only, empty output and skipped
checks do not count as content acceptance.

<!-- Compatibility anchor for earlier links; not a release-status assertion. -->
<a id="coverage-v2-capture-unreleased"></a>

## Coverage-v2 capture (Libra 0.30.50)

From the first stable release containing coverage-v2, OpenCode export capture
and historical import retain supported provider-visible reasoning as typed,
redacted turn content. Turns containing that content use coverage schema 2;
ordinary turns keep schema 1 and their existing canonical bytes. Redaction runs
before either digest or checkpoint projection. This does not authenticate
`signature`, `redactedData`, or `reasoningEncryptedContent`: unproven native
fields remain partial and cannot be archived as trusted artifacts.

Retained visible reasoning is part of the redacted checkpoint and follows its
export, `libra agent push`, and cloud-sync paths when used. There is no separate
flag to exclude that reasoning while retaining other captured content.

After upgrading, an interrupted action whose checkpoint was already committed
with v1 (or an empty legacy revision set) first settles that existing receipt
without adding claims or rewriting its checkpoint. A subsequent **new action**
may capture the visible reasoning once under v2; repeated capture then deduplicates.
The old v1 objects, digests, and claims remain readable and unchanged.

If source classification is rejected after that session already has v2 claims,
the entire new capture/import attempt stops, including new ordinary turns.
Its pending receipt/export work stays retryable; no fallback claims or success
acknowledgment are created. An unchanged over-limit or unsupported source can
keep that session blocked. Keep the recorded data, inspect `libra agent doctor`,
and start a new OpenCode session to continue supported capture. A candidate or
unverified reasoning source cannot turn this failure into complete coverage.
These changes add no command flags and require Libra 0.30.50 or later. OpenCode Session/exporter/model acceptance runs locally on
macOS; it is not a CI integration test.

Repositories opened by Libra 0.30.67 or later carry repository capability 4. Released binaries through 0.30.66 refuse every command on those repositories, including `agent clean`; install 0.30.67 or later before continuing. No existing agent workspace directories are migrated.
