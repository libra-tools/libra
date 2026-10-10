# `libra agent`

Manage external-agent capture for Claude Code, Codex, and OpenCode.

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

The OpenCode parser targets the audited **2.0.26** vocabulary. Its current
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

The managed plugin uses the OpenCode 2.0.26 `setup(ctx)` API, a
location-scoped event subscription and an observe-only tool hook. Refresh it
with `libra agent enable --agent opencode`: status reports old or edited
templates, and duplicate managed copies, as not installed until refreshed.
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
15 seconds (currently 18 seconds), with `SIGKILL` at timeout. Install checks both
plugin layouts, rejects symlinks and special files before reading, preserves
user-owned files and rolls back its canonical update if managed duplicate
removal fails. These checks assume stable directory names; concurrent directory
relocation and edits after the final ownership check remain deferred.
`opencode --pure` / `OPENCODE_PURE=1` disables all external plugins and hook
capture. Local Node/Bun tests use synthetic events and a controlled exporter;
real OpenCode content capture and native reasoning proof require their separate
acceptance gates.

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
| `checkpoint export <id>` | Export a checkpoint's transcript. Redacted by default (no authorization); raw (un-redacted) export requires `--allow-raw --raw` and is recorded in the append-only `agent_audit_log` (`LBR-AGENT-013` when refused without it) |
| `skill search` | Search captured skill events by `--skill`, `--provider`, `--session`, and RFC3339 `--since`/`--until` (keyset-paginated with `--limit`/`--cursor`, `--json`). A read-time projection over checkpoint metadata — no dedicated table |
| `skill list` | Alias of `skill search` (same filters) |
| `skill registry` | Show the curated per-agent discoverable-skill registry (`--provider <slug>` to scope; the public SkillDiscoverer surface) |
| `clean` | Clean up temporary checkpoints from stopped sessions (prune fails closed while a checkpoint write is in flight, the traces ref reaches uncataloged commits, or durable object-index repair remains pending; also drops `object_index` rows made unreachable) |
| `doctor` | Diagnose hook installation and capture state; detect (and with `--repair` fix) checkpoint-store inconsistencies. Its object checks deliberately inspect only this repository's local object directory: unavailable alternate/remote objects are reported conservatively rather than resolving an externally controlled alternate path while producing diagnostics. The read-only `legacy_code_residue` field (human: a "Frozen Code residue" line) reports whether the frozen Code-era paths `.libra/sessions/code/` and `.libra/code/` and the `libra/intent` ref still exist; it never deletes or rewrites that state — cleanup is tracked separately (plan-20260920 ADR-RC-04 / DEFER-RC-02) |
| `push` | Push `refs/libra/traces` to a remote (`--force-rewrite` for the non-fast-forward push after a `clean` prune, using force-with-lease) |
| `rpc list` | List discovered `libra-agent-*` binaries on `PATH` (with trusted/quarantined state); requires the external-agents opt-in |
| `rpc trust <slug>` | Trust a discovered binary — records path + sha256 + device/inode/mtime provenance (refused when its directory is world-writable, or when the binary is not under a trusted directory — `LBR-AGENT-005`). The provider-exporter slug `opencode` instead pins the provider's own CLI binary — resolved only from registered trusted directories, never `$PATH` — for the sandboxed export bridge; this form needs no external-agents opt-in. Provenance is hashed in one streaming pass with no fixed size cap, so large single-file provider CLIs (the OpenCode Bun build is ~171 MiB) stay trustable; a file that yields more bytes than its reported size while being hashed is refused (`LBR-AGENT-005`) |
| `rpc trust --dir <path>` | Register a trusted directory (`agent.external_agents.trusted_dirs`, default `~/.libra/agents`): external binaries are only trustable when their canonical path lives under one. The path is canonicalized and must be an existing, non-world-writable directory |
| `rpc untrust <slug>` | Revoke trust; the binary returns to quarantine (always available, even while external agents are disabled) |
| `rpc invoke` | Invoke one JSON-RPC method on a trusted `libra-agent-*` binary |
| `bridge --stdio` | Run the repository-scoped DeepSeek Harness bridge over stdin/stdout (JSON-RPC 2.0 NDJSON). The **only** standard inbound write transport for Harness; it is not `libra code --control`. stdout carries exactly one protocol frame per response; diagnostics go to stderr. Protocol v1: 20-method allowlist, 256 KiB frame cap, 64 in-flight requests, 30 s default deadline. All 20 methods are implemented: the `initialize` handshake; session/event ingress `session.open`, `event.append` (batch ack / idempotent / digest-conflict / server-side redaction), `session.flush`, `session.close`, `evidence.append`, `provenance.append`; the read methods `context.get`, `status.get`, `history.search`, `checkpoint.list`, `checkpoint.show` and `diff.get`; the mutations `checkpoint.create`, `commit.create`, `checkpoint.restore` and `review.run` (each with `operation_id` idempotency, actor binding and approval gating); and workspace lease `workspace.claim` / `workspace.renew` / `workspace.release` (owner derived from the authenticated bridge session). `diff.get` takes a closed `mode` (`worktree` / `staged` / `checkpoint`) plus validated repository-relative `paths` — never a free-form revision or pathspec — and forces `--no-ext-diff` / `--no-textconv` so repository config cannot become process execution. `commit.create` commits the current index only (no `-a`, no pathspec, no amend, no author override) and records its association graph in `agent_bridge_link` rather than in the commit message. `checkpoint.restore` requires an explicit `expected_head` fence plus a clean index/worktree and never moves HEAD. `review.run` starts a read-only review and returns its `run_id`; replaying the same `operation_id` reports that run's state instead of starting a second one. A stale HEAD or dirty worktree is refused with `LBR-AGENT-038` before any write. An ack only means the redacted projection is durable, never that the Harness raw transcript has been migrated. Not a Git command. |

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

## Scoped checkpoint resume

`review --checkpoint <id>` and `investigate start --checkpoint <id>` still materialize ordinary checkpoint files, read-only, under `<run_dir>/checkpoint-input/`. `investigate continue` writes that saved spec again only when the explicit repository catalog's ordinary leaves have the same paths and blob ids. A path `reasoning/encrypted/<64 hex>` is not ordinary input: it is not written, and a saved spec that names it is refused before the previous input directory is cleared. A saved id that is not a blob, or whose bytes do not hash to that id, is refused before that directory is cleared. If the catalog cannot be opened, queried, or closed, a paused continue returns the existing store error and leaves that run's state and input bytes unchanged. A zero review setup budget ends as the existing infrastructure error and does not launch reviewers; cancel ends as cancelled. No new stable error code is added. Ordinary payload bytes stay capped at 64 MiB per file and 256 MiB total. Each catalog tree read is capped at 16 MiB, separate from those payload caps.

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

On macOS, OpenCode **content export is unsupported**: Seatbelt cannot give the
bridge cancellation-safe containment for a forking exporter, so Libra fails
closed before it spawns `sandbox-exec` or the exporter and hook capture stays
metadata-only. Linux uses required bubblewrap containment. To enable the
Linux exporter, register the directory containing the verified
`opencode` binary and then pin it: `libra agent rpc trust --dir <path>`
followed by `libra agent rpc trust opencode`. Neither step opens the
external-RPC surface (`agent.external_agents.enabled` stays untouched); the
binary is resolved only from registered trusted directories, never `$PATH`.
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
  `libra worktree repair --confirm <worktree-path>` remedy), `LBR-REPO-002`
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
- Managed Session Capture currently requires Unix for secure repository-private
  deduplication-key initialization. On non-Unix platforms it fails closed
  before creating key files, with an actionable diagnostic, rather than using
  a weaker path-based publication fallback. The fixed capability diagnostic is
  explicit for both Codex entry points: nonterminal `hooks codex` callbacks
  (including the hidden legacy alias) print the path-free Unix-host remedy to
  stderr but exit `0`; `SessionEnd` returns non-zero with that same remedy.
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

Artifact tree reachability is added by RG-06; controlled read and export
surfaces are added by RG-03.


Checkpoint readers verify the object type, content-addressed identity and ordinary-role bindings before consuming a manifest or metadata body. Skill search skips metadata that does not belong to the catalog's named checkpoint. The fresh `--checkpoint` resolver uses the explicit repository catalog in read-only mode and refuses missing or inconsistent stores without creating a run. Saved-input resume uses its existing path until the separately tracked recovery card is delivered. Default checkpoint show/list summaries and raw-export authorization/audit remain unchanged. Each skill candidate is checked independently: ordinary child trees are capped at 16 MiB and the selected metadata blob at 16 MiB; the checkpoint binding is checked before walking the ordinary closure.
