# `libra agent`

Manage external-agent capture for tools such as Claude Code and Gemini.

## Synopsis

```bash
libra agent status
libra agent list [--json]
libra agent enable [--agent <name>]...
libra agent add [<name>...]
libra agent disable [--agent <name>]...
libra agent remove [<name>...]
libra agent session <subcommand>
libra agent checkpoint <subcommand>
libra agent clean [--all]
libra agent doctor [--repair]
libra agent push [--remote <name>] [--force-rewrite]
libra agent rpc <subcommand>
```

## Description

`libra agent` manages Libra's external-agent capture surface. It installs and
removes provider hooks, reports captured session/checkpoint state, exposes
read-only diagnostics, and can push `refs/libra/traces` to a remote.

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
| `list` | List agents with their capability matrix (roster, hooks, install state) |
| `enable` | Enable one or more external agents and install hooks |
| `add` | Alias of `enable`: `add <name>` ≡ `enable --agent <name>` |
| `disable` | Disable one or more external agents and uninstall hooks |
| `remove` | Alias of `disable`: `remove <name>` ≡ `disable --agent <name>` |
| `session list` | List captured sessions |
| `session show <id>` | Show a captured session |
| `session stop <id>` | Mark a captured session as stopped |
| `session resume <id>` | Mark a stopped captured session active again |
| `session promote <id>` | Promote a captured session into Libra intent metadata |
| `session derive-tool-calls <id>` | Derive tool-call records from a captured session |
| `checkpoint list` | List captured checkpoints |
| `checkpoint show <id>` | Show checkpoint metadata |
| `checkpoint rewind <id>` | Inspect or apply a working-tree rewind for one checkpoint |
| `checkpoint export <id>` | Export a checkpoint's transcript. Redacted by default (no authorization); raw (un-redacted) export requires `--allow-raw --raw` and is recorded in the append-only `agent_audit_log` (`LBR-AGENT-013` when refused without it) |
| `clean` | Clean up temporary checkpoints from stopped sessions (prune fails closed while a checkpoint write is in flight or the traces ref reaches uncataloged commits; also drops `object_index` rows made unreachable) |
| `doctor` | Diagnose hook installation and capture state; detect (and with `--repair` fix) checkpoint-store inconsistencies |
| `push` | Push `refs/libra/traces` to a remote (`--force-rewrite` for the non-fast-forward push after a `clean` prune, using force-with-lease) |
| `rpc list` | List discovered `libra-agent-*` binaries on `PATH` (with trusted/quarantined state); requires the external-agents opt-in |
| `rpc trust <slug>` | Trust a discovered binary — records path + sha256 + device/inode/mtime provenance (refused when its directory is world-writable) |
| `rpc untrust <slug>` | Revoke trust; the binary returns to quarantine (always available, even while external agents are disabled) |
| `rpc invoke` | Invoke one JSON-RPC method on a trusted `libra-agent-*` binary |

## Common Options

| Flag | Subcommand | Description |
|------|------------|-------------|
| `--agent <name>` | `enable`, `disable` | Select agent names; omit to target the supported roster (`add`/`remove` take the names positionally) |
| `--limit <n>` | `session list`, `checkpoint list` | Maximum rows per page (default 50, hard cap 500 — larger values clamp with a stderr note; `0` is treated as `1`) |
| `--cursor <cursor>` | `session list`, `checkpoint list` | Opaque keyset cursor from the previous page's `next_cursor`; do not construct by hand |
| `--extract-transcript <path>` | `session show` | Copy the captured transcript path from session metadata to a local file |
| `--all` | `clean` | Clean all stopped-session checkpoints instead of only the most recent |
| `--repair` | `doctor` | Repair detected checkpoint-store inconsistencies (rebuild stale/missing catalog rows from `refs/libra/traces`, re-enqueue missing `object_index` rows); detection-only when omitted |
| `--remote <name>` | `push` | Select the remote used for pushing agent trace refs |
| `--force-rewrite` | `push` | Allow the non-fast-forward push that follows a local `clean` prune (the traces ref is Libra-managed and rewritten as a whole chain); uses force-with-lease against the last tip this repository pushed — never an unconditional force — so a remote rewritten elsewhere still fails closed |
| `--dry-run` | `checkpoint rewind` | Show the impact without modifying files; this is the default |
| `--allow-raw` / `--raw` | `checkpoint export` | Authorize + request a raw (un-redacted) export; without `--allow-raw` a `--raw` request is refused (`LBR-AGENT-013`) and audited |
| `--justification <text>` / `-o <path>` | `checkpoint export` | Audit justification and output file for a raw export |
| `--gc` / `--retention-days <n>` | `clean` | Retention GC: drop checkpoints from stopped sessions older than `agent.retention.transcript_days` (default 90; override with `--retention-days`), and prune reviewer stderr diagnostic logs of terminal review/investigate runs older than `agent.retention.stderr_days` (default 30) while keeping each run's aggregate record (`state.json`/`manifest.json`/`findings.md` + stdout logs); never touches `agent_audit_log` |
| `--apply` | `checkpoint rewind` | Restore the working tree for the selected checkpoint |

## JSON Output

Subcommands that support structured output use the global `--json` and
`--machine` envelope. For example:

```bash
libra --json agent status
libra --json agent list
libra --json agent checkpoint list
libra --json agent rpc list
```

`agent list --json` carries a stable `schema_version` plus one row per known
agent (`slug`, `agent_kind`, `stability`, `supported`, `support_wave`,
`registered`, `transcript_readable`, `hook_installable`, `installed`,
`launchable_review`, `launchable_investigate`, `external_binary`,
`config_paths`, `protected_dirs`, `capabilities`). The row shape is a frozen
contract for automation.

`agent session list --json` and `agent checkpoint list --json` return one
page per call: `data` carries a `schema_version`, the rows under `sessions`
/ `checkpoints` (per-row shape unchanged), and `next_cursor` — an opaque
token to pass back via `--cursor`, `null` once the listing is exhausted.
Pages are ordered newest-first (`started_at` / `created_at` descending,
with the row id as tiebreaker).

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

## Notes

- External `libra-agent-*` agents are **disabled by default**. Opt in with
  `libra config set agent.external_agents.enabled true` (repo-local); until
  then `rpc list`/`rpc trust`/`rpc invoke` refuse with `LBR-AGENT-002`
  (`rpc untrust` stays available — revoking trust only tightens security).
  Discovered binaries stay quarantined until `rpc trust <slug>` records
  their provenance (trust is refused for a binary in a world-writable
  directory), every invoke revalidates it (drift revokes trust,
  `LBR-AGENT-005`), the child environment is cleared to an allowlist, and
  stderr is captured/capped/redacted — never inherited. Invoke timeouts,
  broken pipes and malformed frames map to `LBR-AGENT-012`; IO hard-cap
  violations map to `LBR-AGENT-007`.


- The top-level `agent hooks` entry is hidden and intended for hook configs
  installed by `libra agent enable`; users normally do not call it directly.
- `checkpoint rewind --apply` restores working-tree files only; the agent's own
  transcript file is not rewritten.
- Hook and capture diagnostics are best-effort and are designed to report
  actionable installation state rather than silently ignoring missing providers.

### Doctor checkpoint-store repair (`--repair`)

`libra agent doctor` scans the checkpoint store for three inconsistency
classes (AG-20 repair matrix); without `--repair` it is strictly read-only
and reports what `--repair` would do:

| `inconsistency_type` | Meaning | `--repair` action |
|----------------------|---------|-------------------|
| `stale_catalog_row` | An `agent_checkpoint` row's `traces_commit`/`tree_oid`/`metadata_blob_oid` disagree with the checkpoint still reachable from `refs/libra/traces` | Rebuild the row's OID columns from the ref (idempotent UPDATE) |
| `missing_objects` | Checkpoint objects genuinely missing from the store (and the ref cannot rebuild them) — the check covers the full E4 tree: `manifest.json`, `events/lifecycle.jsonl`, `transcript/<agent_kind>.jsonl` incl. chunks, `redaction_report.json`, `content_hash.txt`, the intermediate trees, and every manifest-declared blob | None — reported `manual_required`; doctor never takes destructive action (try `libra fsck --heal` or a cloud/backup restore) |
| `missing_catalog_row` | A checkpoint reachable from `refs/libra/traces` has no catalog row (crash window B) | Re-INSERT the row via the writer's probe-first idempotent path, reconstructed from the commit's `metadata.json` (v1 and v2 shapes) |
| `missing_object_index` | Checkpoint objects missing from `object_index` (invisible to `libra cloud sync`) — covers the traces commit plus the full E4 object set | Idempotent re-insert with the writer's row semantics (trees as `tree`, transcript blobs as `agent_transcript`, sidecars as `blob`) |

Additional rules:

- **Legacy-v1 checkpoints** (pre-AG-20 layout without `manifest.json`) are
  counted in `legacy_v1_checkpoints`, never enter the three classes, and are
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
