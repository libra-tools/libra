# `libra agent`

Manage external-agent capture for Claude Code, Codex, and OpenCode.

## Synopsis

```bash
libra agent status
libra agent list [--schema-version <1|2>] [--json]
libra agent import (--session <id> | --path <path> | --since <rfc3339> | --all) [--agent <name>] [--limit <n>] [--cursor <n>] --yes
libra agent graph <session> [--repo <path>]
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

## Subcommands

| Subcommand | Description |
|------------|-------------|
| `status` | Report captured external-agent session status |
| `list` | List the supported agents with their capability matrix (roster, hooks, install state) |
| `import` | Discover and import historical Claude/Codex transcript files or one trusted, sandboxed OpenCode export after explicit consent |
| `graph <session>` | Browse the read-only session → turn → revision → subagent capture graph; use global `--json`/`--machine` outside a terminal |
| `enable` | Enable one or more external agents and install hooks |
| `add` | Alias of `enable`: `add <name>` ≡ `enable --agent <name>` |
| `disable` | Disable one or more external agents and uninstall hooks |
| `remove` | Alias of `disable`: `remove <name>` ≡ `disable --agent <name>` |
| `session list` | List captured sessions |
| `session show <id>` | Show a captured session, including a non-sensitive retryable checkpoint-capture diagnostic when the last Codex Stop failed |
| `session stop <id>` | Mark a captured session as stopped |
| `session resume <id>` | Mark a stopped captured session active again |
| `session promote <id>` | Promote a captured session into Libra intent metadata |
| `session derive-tool-calls <id>` | Derive tool-call records from a captured session |
| `checkpoint list` | List captured checkpoints |
| `checkpoint show <id>` | Show checkpoint metadata |
| `checkpoint rewind <id>` | Inspect or apply a working-tree rewind for one checkpoint |
| `checkpoint export <id>` | Export a checkpoint's transcript. Redacted by default (no authorization); raw (un-redacted) export requires `--allow-raw --raw` and is recorded in the append-only `agent_audit_log` (`LBR-AGENT-013` when refused without it) |
| `skill search` | Search captured skill events by `--skill`, `--provider`, `--session`, and RFC3339 `--since`/`--until` (keyset-paginated with `--limit`/`--cursor`, `--json`). A read-time projection over checkpoint metadata — no dedicated table |
| `skill list` | Alias of `skill search` (same filters) |
| `skill registry` | Show the curated per-agent discoverable-skill registry (`--provider <slug>` to scope; the public SkillDiscoverer surface) |
| `clean` | Clean up temporary checkpoints from stopped sessions (prune fails closed while a checkpoint write is in flight, the traces ref reaches uncataloged commits, or durable object-index repair remains pending; also drops `object_index` rows made unreachable) |
| `doctor` | Diagnose hook installation and capture state; detect (and with `--repair` fix) checkpoint-store inconsistencies |
| `push` | Push `refs/libra/traces` to a remote (`--force-rewrite` for the non-fast-forward push after a `clean` prune, using force-with-lease) |
| `rpc list` | List discovered `libra-agent-*` binaries on `PATH` (with trusted/quarantined state); requires the external-agents opt-in |
| `rpc trust <slug>` | Trust a discovered binary — records path + sha256 + device/inode/mtime provenance (refused when its directory is world-writable, or when the binary is not under a trusted directory — `LBR-AGENT-005`). The provider-exporter slug `opencode` instead pins the provider's own CLI binary — resolved only from registered trusted directories, never `$PATH` — for the sandboxed export bridge; this form needs no external-agents opt-in |
| `rpc trust --dir <path>` | Register a trusted directory (`agent.external_agents.trusted_dirs`, default `~/.libra/agents`): external binaries are only trustable when their canonical path lives under one. The path is canonicalized and must be an existing, non-world-writable directory |
| `rpc untrust <slug>` | Revoke trust; the binary returns to quarantine (always available, even while external agents are disabled) |
| `rpc invoke` | Invoke one JSON-RPC method on a trusted `libra-agent-*` binary |

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
| `--extract-transcript <path>` | `session show` | Copy the captured transcript path from session metadata to a local file |
| `--all` | `clean` | Clean all stopped-session checkpoints instead of only the most recent |
| `--repair` | `doctor` | Repair detected checkpoint-store inconsistencies (rebuild stale/missing catalog rows, re-enqueue missing `object_index` rows, safely drain valid expired ordinary writer markers, and immediately drain `cleanup_pending` markers regardless of ordinary TTL); malformed markers remain `manual_required`; detection-only when omitted |
| `--remote <name>` | `push` | Select the remote used for pushing agent trace refs |
| `--force-rewrite` | `push` | Allow the non-fast-forward push that follows a local `clean` prune (the traces ref is Libra-managed and rewritten as a whole chain); uses force-with-lease against the last tip this repository pushed — never an unconditional force — so a remote rewritten elsewhere still fails closed |
| `--dry-run` | `checkpoint rewind` | Show the impact without modifying files; this is the default |
| `--allow-raw` / `--raw` | `checkpoint export` | Authorize + request a raw (un-redacted) export; without `--allow-raw` a `--raw` request is refused (`LBR-AGENT-013`) and audited |
| `--justification <text>` / `-o <path>` | `checkpoint export` | Audit justification and output file for a raw export |
| `--gc` / `--retention-days <n>` / `--dry-run` | `clean` | Retention GC across three windows: (1) drop checkpoints from stopped sessions older than `agent.retention.transcript_days` (default 90; override with `--retention-days`); (2) prune reviewer stderr diagnostic logs of terminal review/investigate runs older than `agent.retention.stderr_days` (default 30) while keeping each run's aggregate record; (3) **A0-09** remove whole terminal review/investigate run directories (`findings.md`, `manifest.json`, `state.json`, reviewer logs) older than `agent.retention.findings_days` (default 90). The objectized findings blob is content-addressed and reclaimed by the repo-level `libra maintenance run --task gc` reachability pass (PD-04), which prunes orphaned findings blobs together with their `object_index` rows while live-run manifests keep their OIDs alive (per-run retention never deletes a shared object). Non-terminal/undated runs are skipped fail-safe; `agent_audit_log` is never touched. `--dry-run` reports what each window and companion cleanup *would* remove (including JSON `findings_runs_pruned` and `import_identities_pruned`) without deleting anything |
| `--apply` | `checkpoint rewind` | Restore the working tree for the selected checkpoint |

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
To enable the exporter, register the directory containing the verified
`opencode` binary and then pin it: `libra agent rpc trust --dir <path>`
followed by `libra agent rpc trust opencode`. Neither step opens the
external-RPC surface (`agent.external_agents.enabled` stays untouched); the
binary is resolved only from registered trusted directories, never `$PATH`.
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
or coverage digests. In the interactive TUI, selecting a session, turn,
revision, or subagent additionally shows a bounded content summary: event
counts plus compact user/assistant message previews from the linked
checkpoint. The TUI reads at most 256 KiB per checkpoint, follows the stored
manifest, and applies the default redactor again; it never renders the full
transcript or provider file path. A locally erased session succeeds with `state="erased"`, a
null session, empty turns, and unavailable subagents; it is not recreated.
An id absent from both the session catalog and erasure tombstones fails with
`LBR-AGENT-021`. Without global `--json` or `--machine`, stdin and stdout must
both be terminals; non-interactive callers receive a usage error before TUI
initialization.

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
discovery, is checked during traversal, parsing, reservation, object building,
and CAS persistence. Libra does not cancel an SQLite commit after it starts:
it checks the deadline immediately before each commit, then observes that
commit to a definite outcome. A fully committed final turn is therefore
reported as success even if the clock crosses the deadline while the commit
result is being observed; every uncommitted lease/marker is abandoned on a
deadline failure. If that recovery transaction itself fails, the command
chains the cleanup error and an actionable `agent doctor --repair` hint instead
of reporting only the original failure.
Discovery traversal/open and authorized file-descriptor reads run in private,
kill-on-timeout helper processes (the read helper consumes the already-open
descriptor) under the same absolute deadline; a blocked filesystem operation
cannot silently extend the 120-second command budget. Child discovery receives
a shorter sub-deadline that reserves time for the independently valid parent
checkpoint. Parent-side helper response decoding and completeness validation
recheck that sub-deadline while walking the bounded response, so a late large
child result degrades the parent to partial evidence instead of suppressing its
checkpoint.
After consent, provider preparation (including secure open, parsing,
redaction, and cwd/storage ownership checks), checkpoint loose-object writes,
and the commit/tree reads used while splicing the traces ref also run behind
killable helpers. Object reads accept only the requested `commit`/`tree` type,
verify the full OID and declared length, and enforce a 16 MiB inflated-payload
cap before allocation, so a hostile compressed object cannot turn the command
deadline into unbounded memory use.

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

`agent session list --json` and `agent checkpoint list --json` return one
page per call: `data` carries a `schema_version`, the rows under `sessions`
/ `checkpoints` (per-row shape unchanged), and `next_cursor` — an opaque
token to pass back via `--cursor`, `null` once the listing is exhausted.
Pages are ordered newest-first (`started_at` / `created_at` descending,
with the row id as tiebreaker).

The human `agent session list` table renders `started_at` as a relative age
against the current machine clock (for example, `2 hours ago`). JSON output
keeps the raw Unix timestamp for automation.

Each checkpoint row carries a `scope`. `committed` checkpoints are written at
turn/session boundaries (`Stop` / `SessionEnd`) and carry the redacted
transcript snapshot. There are two deliberately distinct `subagent` evidence
types. Codex `SubagentStart` / `SubagentEnd` hooks create empty-transcript
**boundary** checkpoints linked structurally through `parent_checkpoint_id`.
Claude `<session>/subagents/*.jsonl` files create independent **content**
checkpoints whose `parent_checkpoint_id` remains null: an opaque SHA-256 claim
derived from the securely opened provider-root-relative source and append-only
revisions select one current content leaf without persisting the local project
slug or filename and without rewriting physical history. Claude does not emit a stable boundary identifier,
so its content normally remains `link_state=unresolved`; `agent doctor` reports
that fact and never guesses a match. A provider-stable identifier links content
to a unique boundary through a separate association row, without changing the
checkpoint's immutable traces commit. Both evidence types remain listable,
showable, exportable, prunable, and doctor-visible. An empty or metadata-only
child file has no normalized turn evidence and is therefore recorded as
`partial`, never as a complete child transcript.

Cloud mirroring publishes the capture catalog as dependency-ordered batches
(`session → checkpoint → revision → link → claim`) under a token-fenced
generation. Session, checkpoint, link, and mutable claim projections have independent
monotonic sync generations, so pruning a current child leaf cannot create an
equal-generation conflict and pruning a retained traces chain cannot be undone
by a stale clone. An explicit `--restore-erased` import starts a new durable
replication incarnation for both the session and opaque child-source namespace;
this avoids key reuse while remote deletion propagation remains deferred. The generation also binds a canonical digest of the
remote object index; sync rechecks every checkpoint object in D1 and R2, and
restore accepts the catalog only when the manifest and object-index digest are
unchanged before and after the read. Current clients use versioned v2 remote
session/checkpoint tables. Once v2 is activated, D1 triggers reject legacy
unfenced writers with an upgrade error rather than allowing them to invalidate
a coherent restore snapshot.

`agent checkpoint show --json` additionally reports a `layout` summary
(`e4-libra`, `legacy-v1` for pre-AG-20 checkpoints, or `unknown` when the
checkpoint tree is not locally readable) with the manifest roles, the
transcript parts in manifest order, a `content_hash` format check, and a
transcript `availability` flag (`present`/`missing`/`unknown`) — derived
without reading transcript blob bodies.

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

# Browse one captured session's turn/revision/subagent structure
libra agent graph <session-id>

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

# Show a session and copy its captured transcript
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

# Show a single checkpoint by id
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


- The top-level `agent hooks` entry is hidden and intended for hook configs
  installed by `libra agent enable`; users normally do not call it directly.
  A hook envelope that fails size / UTF-8 / JSON / schema / transcript-path
  validation is rejected fail-closed with `LBR-AGENT-008` (exit 128) — raw
  stdin is never echoed. A checkpoint operation (e.g. `checkpoint rewind`) on
  an inconsistent store — a catalog row whose `parent_commit` is malformed or
  points at a missing traces object — fails with `LBR-AGENT-009` (exit 128);
  run `libra agent doctor` to inspect the store.
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
| `missing_objects` | Checkpoint objects genuinely missing from the store (and the ref cannot rebuild them) — the check covers the full E4 tree: `manifest.json`, `events/lifecycle.jsonl`, `transcript/<agent_kind>.jsonl` incl. chunks, `redaction_report.json`, `content_hash.txt`, the intermediate trees, and every manifest-declared blob | None — reported `manual_required`; doctor never takes destructive action (try `libra fsck --heal` or a cloud/backup restore) |
| `missing_catalog_row` | A checkpoint reachable from `refs/libra/traces` has no catalog row (crash window B) | Re-INSERT the row via the writer's probe-first idempotent path, reconstructed from the commit's `metadata.json` (v1 and v2 shapes) |
| `missing_object_index` | Checkpoint objects missing from `object_index` (invisible to `libra cloud sync`) — covers the traces commit plus the full E4 object set | Idempotent re-insert with the writer's row semantics (trees as `tree`, transcript blobs as `agent_transcript`, sidecars as `blob`) |
| `expired_inflight_marker` | A valid traces writer marker outlived its TTL, including provisional preclaims and proven-created loose-object OIDs | Fence the expired writer in the final ref transaction, run the serialized repository-root proof, and retire the marker; inline recovery never unlinks shared payloads or removes `object_index` rows, leaving physical reclamation to repository GC |
| `invalid_inflight_marker` | Marker JSON, row identity, commit, or OID is malformed | None — reported `manual_required`; automatic deletion is unsafe because ownership cannot be decoded |
| `conflicted_coverage_claim` | Two different complete payloads claimed the same logical turn; doctor reports only a hashed turn identity plus the incumbent revision/digest and the retained first challenger's digest/source/time/redaction evidence | None — reported `manual_required`; inspect both durable sanitized candidates and choose an explicit recovery rather than silently discarding provenance. `--repair` never chooses a winner |
| `inconsistent_subagent_content` | A current subagent content claim is missing or disagrees with its immutable revision, checkpoint catalog row, or association link | None — reported `manual_required`; unchanged replay fails closed until the companion relation and checkpoint objects/ref reachability are restored |
| `unresolved_subagent_link` | A current subagent content checkpoint has no unique provider-stable boundary match (the normal Claude case) | None — reported `manual_required`; content and boundary evidence stay independently preserved and doctor never guesses an association |

Additional rules:

- **Legacy-v1 checkpoints** (pre-AG-20 layout without `manifest.json`) are
  counted in `legacy_v1_checkpoints`, never enter checkpoint-object repair classes, and are
  never rewritten by `--repair`.
- Checkpoints named by a **live traces in-flight marker** are writers
  mid-flight, not inconsistencies, and are skipped.
- A **session without checkpoints is legal** (an active session before its
  first stop) and is never flagged; only checkpoint-without-session counts
  as an orphan.
- Captured **gemini rows stay readable** and are never flagged; leftover
  gemini hook *configuration* produces a hint pointing at the
  uninstall-only channel (`libra agent remove gemini`).
- All repairs are idempotent — running `doctor --repair` twice performs no
  work the second time. With `--repair`, one `agent.doctor.repair` tracing
  span is emitted per repair attempt (`inconsistency_type`, `repaired`,
  `manual_required`); transcript content never reaches the log.
