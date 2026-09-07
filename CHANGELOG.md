# Changelog

## [Unreleased]

### Changed: `diff` scan progress is deferred, TTY-gated, and self-erasing (#466)

`libra diff` no longer prints `Scanning working tree ...` on every invocation.
The working-tree scan is a fast local read (a normal tree finishes in tens of
milliseconds), so the line is now a liveness cue only:

- It appears only when the scan has actually been running for more than 2
  seconds (very large working trees), and is erased when the scan completes —
  no more terminal scrollback residue.
- Under the default `--progress=auto` it is TTY-gated: stderr redirected to a
  file or pipe (CI logs, `2>&1`) never receives the line or any ANSI escape,
  matching git's output conventions.
- It never fires for `--staged`, rev-vs-rev comparisons, `--quiet`, or
  `--json`/`--machine` runs — it only describes the unstaged working-tree scan.
- `--progress=json` keeps emitting immediate `diff_scan.start` NDJSON events;
  `--progress=none` suppresses everything. An explicit `--progress=text` with
  redirected stderr receives the bare line text (no escape bytes).

### Changed: SSH host key handling now matches Git's transport behavior

`libra clone`/`fetch`/`push`/`ls-remote` over SSH no longer force
`StrictHostKeyChecking=yes` + `BatchMode=yes`. In the new default `ask` mode
libra passes **no** `StrictHostKeyChecking` option to `ssh`, so the user's
`~/.ssh/config` governs and OpenSSH offers its interactive trust prompt (TOFU)
on first connection — the same experience as `git clone` on a fresh machine
(no more "No ED25519 host key is known for …" hard failure before the host key
has been accepted once).

- Interactive sessions (TTY on stdin): stderr is inherited, so the
  authenticity warning, fingerprint, and "Permanently added" confirmation are
  visible live; headless contexts (CI, agents, tests) keep the previous
  piped-stderr diagnostics and gain `BatchMode=yes` so ssh fails fast instead
  of prompting.
- `ssh.strictHostKeyChecking` / `LIBRA_SSH_STRICT_HOST_KEY_CHECKING` now
  accept the four OpenSSH policies: `ask` (default), `yes`, `accept-new`, `no`.
- Host key verification failures in clone discovery now surface a targeted
  hint (`ssh -T <host>`, `ssh-keyscan`, or `accept-new`) instead of the generic
  network hint.

## [0.22.0] — 2026-08-30

### Removed (breaking): the SSE wire v1 snapshot stream (plan-20260824 DF-08, ADR-DF-03)

`GET /api/code/events` now serves only the **v2 delta/cursor wire**. Explicit
`?wire=1` / `?wire=v1` (or `Accept: text/event-stream;libra-wire=1`) returns
`400 INVALID_WIRE_VERSION` with removal guidance, and the `libra code
--control stdio` client no longer falls back to v1 when a session has no
durable store — the exact `503 WIRE_V2_REQUIRES_DURABLE_SESSION` surfaces to
the caller instead.

**Upgrade path:** **`v0.21.29` is the last release serving wire v1** — stay on
exactly that release if you cannot consume v2 yet (it is the DEP-02-baked
build: consumers migrated and the server default already v2, with explicit v1
still served; earlier 0.21.x builds predate parts of that bake). To migrate, request `?wire=2&cursor=<last acknowledged
cursor>`, persist the cursor, deduplicate side effects by `eventId`, and treat
`event: resync` / `WIRE_V2_RESYNC_REQUIRED` as one explicit
`GET /api/code/session` snapshot fetch followed by a reconnect at the
server-provided `durableTail` (see `docs/commands/code.md`). The removal
followed the completed DEFER-08 bake: consumers migrated (DF-05), server
default flipped in `v0.21.28` (DF-06, last default-v1 release `v0.21.27`), and
one further public patch release (`v0.21.29`) shipped with v1 still working.

## [0.21.28] — 2026-08-29

### Changed: the server-side SSE default wire is now v2 (plan-20260824 DF-06)

`GET /api/code/events` without `wire=` / `Accept: text/event-stream;libra-wire=N`
now negotiates the **v2 delta/cursor stream**. **`v0.21.27` was the last release
whose omitted-wire default was v1.** Explicit `?wire=1` / `v1` still returns the
full-snapshot stream (unchanged), and illegal values still fail closed with
`INVALID_WIRE_VERSION`. Clients that relied on the omitted-wire v1 default must
either pin `?wire=1` (supported until the DEFER-08 removal checklist completes)
or consume v2 (`wire=2&cursor=<last>`; see `docs/commands/code.md`).

## [0.21.1] — 2026-08-24

### Added: the bridge's remaining VCS methods (plan-20260818 LB-04/LB-05, closes DEFER-LB-07)

`0.21.0` shipped `libra agent bridge --stdio` with `diff.get` returning a stable
not-implemented error and `commit.create` / `review.run` / `checkpoint.restore`
passing admission but failing closed before any VCS service. All four are now
wired through a typed adapter (`src/internal/ai/agent_bridge/vcs.rs`), so the
v1 20-method allowlist is implemented end to end.

- **`diff.get`** returns the real working-tree, staged, or checkpoint-scoped
  diff. Selectors stay typed: a closed `mode` enum (`worktree` / `staged` /
  `checkpoint`) plus validated repository-relative `paths` — never a free-form
  revision or pathspec magic. `checkpoint` mode resolves its revision from the
  bridge's own `agent_bridge_checkpoint` row (or its linked
  `agent_checkpoint.parent_commit`), so a request can never name an arbitrary
  commit. The diff forces `--no-ext-diff` / `--no-textconv`, so repository
  configuration cannot turn a bridge read into process execution (GC-LB-03).
  Results are bounded by the page cap and a shared patch budget, with explicit
  `diff_truncated` / `diff_patch_budget_exhausted` warnings.
- **`commit.create`** commits the current index — no `-a`, no pathspec, no
  amend, no author override — and records its association graph (operation,
  workspace, parent session, evidence ids) as `agent_bridge_link` rows rather
  than splicing metadata into the commit message. An optional `expected_head`
  is verified before the commit is created.
- **`checkpoint.restore`** restores the working tree to the commit a bridge
  checkpoint pins, reusing the same typed path as
  `libra agent checkpoint rewind --apply`. It requires an explicit
  `expected_head` fence **and** a clean index/worktree, and never moves HEAD.
- **`review.run`** validates the reviewer roster against the launchable
  capability matrix, resolves an optional checkpoint scope and takes an
  agent-run admission slot — all before any run directory exists — then runs the
  read-only review under a supervisor. When the bridge's stdio loop ends the
  supervisor cancels and drains every live run, so no reviewer process group
  outlives the bridge (GC-LB-10).
- **Non-idempotent replay is safe.** Replaying a recorded `operation_id` for
  `commit.create`, `checkpoint.restore` or `review.run` returns the ORIGINAL
  result (and, for a review, its current run state) instead of executing a
  second time — the "response was lost, query by operation id" recovery path.

### Fixed

- **A bridge mutation can record more than one association.**
  `agent_bridge_link` was keyed `UNIQUE(source_type, source_id)`, which allowed
  exactly one association per result: every workspace / parent-session /
  evidence link after the first was reported as a retarget conflict, so the
  rich provenance LB-05 promises could never be persisted.

### Added

- `LBR-AGENT-038` — a bridge mutation's fence drifted (HEAD moved, or the
  index/worktree is dirty where the operation requires a clean one). Refused
  **before** any write, so nothing is partially applied.

### Migration

- `2026082401_agent_bridge_link_relations` rebuilds `agent_bridge_link` with
  edge-level uniqueness `(source_type, source_id, target_type, target_id)` and
  adds the `commit` / `restore` / `review` source kinds. Every existing edge is
  copied verbatim. The down path is forward-only: it freezes while any link row
  exists and never deletes recorded provenance (ER-LB-04).

## [0.21.0] — 2026-08-23

### Added: DeepSeek Harness bridge (`agent bridge --stdio`, plan-20260818 LB-01..LB-06)

- **`libra agent bridge --stdio`** — the repository-scoped DeepSeek Harness JSON-RPC
  2.0 NDJSON ingress. The ONLY standard inbound write transport for the Harness
  plugin; intentionally different from `libra code --control stdio` (a client
  controlling a live Code session) and from any MCP transport. Protocol v1: a
  20-method allowlist, 256 KiB frame cap, 64 in-flight requests, 64-event /
  256 KiB batch, 30 s default request deadline.
- **Durable ingress** (`agent_bridge_session/event/operation/checkpoint/link`
  tables, migration `2026081801`): idempotent `session.open`, bounded
  `event.append` with per-event accepted/duplicate/digest-conflict status and
  monotonic `last_acked_seq`, `session.flush`/`session.close`, and
  `evidence.append`/`provenance.append` association links. Ack means the
  redacted projection is durable — never that Harness took over Libra's raw
  transcript.
- **Server-side redaction** (GC-LB-08): only a bounded, redacted projection is
  persisted; raw prompts/reasoning/tokens/secrets never land in the store, and
  a payload that cannot be safely redacted is refused fail-closed with no raw
  fallback.
- **Typed read methods** (LB-04): `context.get`, `status.get`,
  `history.search`, `checkpoint.list`, `checkpoint.show` over the bridge
  projection with a unified `{schema_version, repository_id, workspace_id,
  operation_id, status, data, warnings}` envelope, bounded pagination, and
  repository-scoped queries (a cross-repository `status.get` cannot leak
  another repo's totals).
- **Mutation admission + approval + actor binding** (LB-05): `checkpoint.create`
  is fully wired; every mutation requires an `operation_id` (idempotent replay,
  digest-conflict fail-closed), binds the trusted session/actor scope, and
  enforces a default-deny approval gate for dangerous actions. `commit.create`,
  `review.run` and `checkpoint.restore` pass admission but fail closed with a
  stable "not wired to the VCS service" error until the service plumbing lands
  (wired in `0.21.1`).
- **Workspace lease** (LB-06): `workspace.claim`/`renew`/`release` route through
  the existing `WorkspaceStore` (owner + monotonic fence); the lease owner is
  derived from the authenticated bridge session identity (never a self-reported
  value), so a stale owner or a forged `agent_id` cannot reclaim another
  session's lease. Dedicated error codes `LBR-AGENT-022` (lease held) and
  `LBR-AGENT-023` (lease lost).
- **Protocol authority** (GC-LB-02): the schema, method allowlist, limits,
  error catalogue and frame shapes live only in
  `src/internal/ai/agent_bridge/protocol.rs`; the TypeScript plugin consumes
  the fixture published from it and must not define a second schema.

### Migration

- `2026081801_agent_bridge_capture` creates the five bridge durable tables.
  Down-migration is forward-only: it freezes while any bridge row exists and
  never deletes acked events/evidence (GC-LB-09 / ER-LB-04).

## [Unreleased]

### Fixed (plan-20260715 W2-03 repair, 2026-08-21, v0.20.4)

- **Default-Web IntentSpec revision is durable and fail-closed.** IntentSpec
  Modify consumes either the next accepted plain-text message or canonical
  `/intent modify <changes>` exactly once. The additive public
  `intent_revision` binding contains only the interaction id and sidecar
  digest; an HMAC-authenticated local Prepared/Active/Claiming/Consuming
  sidecar carries the private recovery copy. Claiming fsyncs the complete
  consumer command binding before Runtime admission, and promotion binds its
  exact event id/sequence before the executor start gate opens. Fresh explicit
  direct commands receive `409 SESSION_BUSY` while revision authority is
  active, but exact terminal/live `commandId` retries remain idempotent
  acknowledgements. Exact raw `/intent cancel` uses the same claim and exact
  consumption receipt before deleting the sidecar, then exits without calling
  a provider or tool; padded spellings receive no privileged handling, its
  terminal retry appends nothing, and restart does not revive the revision.
  A Modify consumer is committed only after exactly one successful
  `submit_intent_draft` call and the replacement IntentSpec review marker are
  durable; zero or multiple submissions preserve the revision and fail closed.
  The 16 KiB Modify suffix is validated before Claiming, and only that suffix
  is sent as the provider change request. Crash recovery never reruns a
  provider after durable replacement proof, repairs stale transcript/tool
  projection, permanently closes the receipt's exact retry lineage, and
  rejects conflicting marker/terminal ordering. Plan Modify separately retains
  its next-plain-text replacement-Plan behavior. The
  revision protocol never copies the raw note into a workflow terminal,
  receipt, or dedicated SSE payload. Ordinary transcript and session-snapshot
  retention remain unchanged. Consumption is committed by the additive
  `intent_revision_consumption` receipt and projected through the dedicated
  SSE v2 `intent_revision_consumed` kind without masquerading as a second gate
  resolution. Crash, retry, stale-authority, and malformed-replay paths now
  either recover the exact revision or require reconciliation.
  Startup builds one shared linear `ValidatedIntentRevisionReceiptIndex` for
  source terminals, receipts, consumer status, review-marker proof, and retry
  lineage instead of rescanning the workflow once per receipt. A 5,000-event
  regression with 700 receipts caps indexed relationship visits at four times
  the replay size.
- **Network Allow admits confirmed plan execution (W2-04).** Allow consumes the
  network-policy gate and submits the reviewed plan onto the serialized
  `AgentRuntime` queue via `submit_confirmed_plan_execution`. Mutating tools
  still pass through the shared hardening/approval/sandbox/ACL boundary;
  classified failures park the W2-11 repair loop. The catalogued
  `PLAN_EXECUTION_NOT_AVAILABLE` 409 is retained for older clients and is no
  longer produced on Allow. Deny, Back, and crash/resume remain usable.
- **Fresh repositories can enter Phase 1 safely.** A named unborn HEAD after
  `libra init` is now captured as a valid checkout binding with no object id;
  detached checkouts and existing branches still require an exact valid id.
- **Phase 1 workspace drift is recoverable without weakening Execute.** Review
  contexts retain an exact content fingerprint for Execute and add a
  body-read-free `metadata-v1:<sha256>` token for fast resume projection and
  determinate pre-write validation/retry signals. It never authorizes Execute.
  New bindings use metadata-before/exact-before/exact-after/metadata-after
  scans and refuse any exact or metadata mismatch rather than pairing
  mismatched authority and drift state, including on platforms with coarse
  file metadata.
  Every scan enforces cooperative 30-second, 1,000,000-traversed-entry, and
  128-MiB encoded-path-name budgets; it streams and caps directory entries
  before deterministic sorting and checks the deadline after blocking calls
  and final EOF. Path-name enumeration remains an ignore-aware walk and is not
  trusted as read authority. On Unix, authoritative entry reads use pinned
  root/parent descriptors with `openat`/`fstatat`/`readlinkat`,
  no-follow/nonblocking opens, and descriptor/path identity revalidation. On
  Windows, pinned handles, `FILE_FLAG_OPEN_REPARSE_POINT`, final-path/file-
  identity checks, and `FSCTL_GET_REPARSE_POINT` protect regular files and
  reparse targets. Other platforms fail closed because secure workspace reads
  are unsupported. A rename, symlink, FIFO, parent/root replacement, or
  reparse race therefore cannot hash content outside the checkout. Legacy
  contexts without the additive token remain readable and fall back to exact
  content comparison. Resume projects
  `workspaceDrifted` / `workspaceWarning`; a
  metadata-only warning may pass Execute's exact recheck, while stale content
  returns
  `409 PHASE1_WORKSPACE_CHANGED` without fencing, resolving the gate, or
  mutating files. Modify recaptures a new HEAD only within the same repository;
  repository replacement requires a new IntentSpec review. Failed recapture
  leaves the note unconsumed, while an empty note returns
  `400 PLAN_REVISION_NOTE_REQUIRED` and keeps revision authority pending.
- **Headless resume has one process-lifetime session writer.** A dedicated
  Phase 1 writer lease is acquired before session reload or projection fold,
  is bound to the exact SessionStore and session id, and lets cloned lease
  tokens construct only one persistence graph. It is distinct from the
  short-lived workflow append lock and from browser controller leases. Unix
  opens use `O_NOFOLLOW | O_NONBLOCK` plus device/inode checks; Windows rejects
  reparse points and verifies volume/file identity. Process exit releases the
  advisory lock immediately, while the persistent lock file is never unlinked.
- **Crash resume uses an ABA-safe workflow append lock.** The append lock is a
  persistent regular file protected by an OS advisory lock; process exit
  releases it immediately, while a live owner is never reclaimed by age.
  Guards never unlink the path, Unix opens use `O_NOFOLLOW`, and symlink or
  special lock entries fail closed without truncating their targets.
- **Phase 1 recovery rejects incomplete authority before cleanup.** Startup
  uses one committed workflow replay for gate validation and context GC;
  sequence gaps or a cut replay window fail closed before authority selection,
  event-log rewriting, or irreversible sidecar deletion.
- **Runtime-owned interaction histories keep their selected durability mode.**
  Deliveries that defer audit to the combined terminal row no longer emit an
  intermediate standalone resolution, while legacy executor responses and
  mutating user-input/approval continuations retain their required checkpoint
  paths.

### Removed (plan-20260715 W5-10, 2026-08-20, v0.20.3)

- **W5-10 removes the retired terminal-UI dependency surface.** Direct
  `ratatui` and `crossterm` dependencies, together with the now-unreachable
  terminal-specific Code runtime branch, are gone. The active Code UI remains
  the Web UI; persisted legacy controller value `"tui"` is only decoded so it
  can be rejected fail-closed and is never emitted for a new session. The
  Rust `internal` module tree is an implementation detail rather than the
  patch-compatible external embedding API. This internal cleanup is not a
  Cargo semver surface and is intentionally released as the v0.20.3 patch.

### Fixed (plan-20260715 W5-10, 2026-08-20, v0.20.3)

- **Web runtime cancellation and ACL hardening.** A browser turn cancelled
  before execution now settles through a bounded admission path; an uncertain
  durable correction fences the session as `indeterminate_side_effect` rather
  than treating it as safe to resume. Interaction-registration retries are
  bounded, and dynamically prefixed SourcePool tools remain default-denied in
  Review and Research contexts.

### Breaking (plan-20260715 W5-09, 2026-08-17, v0.20.0)

- **v0.20.0 is the single breaking release of the W5-01 family**
  (sub-cards W5-01/W5-06/W5-07/W5-08 + this release point). It deletes
  every public surface deprecated during the W4 bake window in one atomic
  step; the last release that still supports the removed surfaces is
  **v0.19.158**. Removed:
  - `libra code-control` forwarding shim (W5-01) — migrate to
    `libra code --control stdio` (`--control-url` / `--control-token-file`
    replace the legacy `--url` / `--token-file`).
  - Legacy Code TUI startup path, including the bare
    `--provider codex --resume` TUI resume driver (W5-06) — every
    `libra code` launch apart from the MCP `--stdio` entry and the
    `--control stdio` automation client (a client-only shim, no UI boot)
    launches the Web Code UI; bare codex+resume fails with a usage error
    plus a migration hint; `--network-access allow` is rejected in every
    mode until the Plan network-policy gate owns per-execution sandbox
    network.
  - `--web` / `--web-only` aliases and the hidden
    `LIBRA_CODE_LEGACY_TUI` rollback env (W5-07) — the bake-period
    rollback capability terminates with this release; the env no longer
    changes any behavior.
  - Interactive graph TUI entry of `libra graph` / `libra agent graph`
    (W5-08) — bare invocations fail with `LBR-CLI-002` (exit 129) plus a
    migration hint; `--json` / `--machine` output is unchanged.
  The four sub-cards were held as local-only commits under the family's
  no-push window and are published together here; the family counts as
  exactly **one** public release. Per-card migration details follow in
  the W5-01/W5-06/W5-07/W5-08 entries below and in
  `docs/commands/code.md`, `code-control.md`, and `graph.md`.

### Removed (plan-20260715 W5-01, 2026-08-15, v0.20.0)

- **W5-01 removes the deprecated `libra code-control` forwarding shim.**
  The binary now rejects `code-control` as an unknown command (stable
  CLI-error exit 129). The canonical stdio automation client is
  `libra code --control stdio` (JSON-RPC NDJSON; endpoint discovered from
  `.libra/code/control.json` by default; `--control-url` /
  `--control-token-file` replace the legacy `--url` / `--token-file`).
  Migration: see `docs/commands/code-control.md`.

### Removed (plan-20260715 W5-06, 2026-08-17, v0.20.0)

- **W5-06 removes the legacy Code TUI startup path from `libra code`.**
  Every `libra code` launch apart from MCP `--stdio` and the
  `--control stdio` client shim now launches the Web Code UI: the
  `execute_tui` startup path, the `TuiCodeUiAdapter`, terminal
  alternate-screen/panic-hook setup, and the follow-up
  `libra graph --json <thread_id>` printed on TUI exit are gone. Bare
  `libra code --provider codex --resume <thread_id>` no longer dispatches
  to the legacy TUI resume driver — it fails with a clap usage error plus
  a migration hint (managed Codex Web resume has not landed; Web
  `--provider codex` still rejects `--resume`), while non-Codex Web
  `--resume <thread_id>` in the same working directory is unchanged. With
  no TUI mode left that accepted it, `--network-access allow` is now
  rejected in every mode until the Plan network-policy gate owns
  per-execution sandbox network (W5-09 aggregates the release note for the
  whole W5-01 family; no version bump here). Migration: use plain
  `libra code` (Web Code UI); resume threads with a non-Codex Web
  `--resume <thread_id>` in the same working directory; approve network
  through the Plan review gate instead of `--network-access allow`.

### Removed (plan-20260715 W5-07, 2026-08-16, v0.20.0)

- **W5-07 removes the deprecated `--web` / `--web-only` aliases and the
  hidden `LIBRA_CODE_LEGACY_TUI` rollback switch from `libra code`.** The
  Web Code UI has been the default since W4-01; the old flags now fail with
  a clap usage error plus a migration hint pointing at plain `libra code`,
  and the environment variable no longer changes any behavior. The
  bake-period rollback capability ends with this change (W5-09 aggregates
  the release note for the whole W5-01 family; no version bump here).
  Migration: drop `--web` / `--web-only` and unset `LIBRA_CODE_LEGACY_TUI`;
  plain `libra code` already opens the Web Code UI. The bare
  `libra code --provider codex --resume <thread_id>` TUI resume driver was
  later removed by W5-06 (see the W5-06 entry above).

### Removed (plan-20260715 W5-08, 2026-08-16, v0.20.0)

- **W5-08 removes the interactive TUI entry of `libra graph` and
  `libra agent graph`.** Bare `libra graph <THREAD_ID>` /
  `libra agent graph <session>` now fail with a stable usage error
  (`LBR-CLI-002`, exit 129) plus a migration hint, refused deterministically
  before any projection/capture load. The live thread version graph view
  lives in Web Code UI (`libra code`, delivered in W4-04); the structured
  `libra graph --json` / `--machine` output and the frozen capture-graph JSON
  v1 schema of `libra --json agent graph` are unchanged and remain the
  agent/automation path (W5-09 aggregates the release note for the whole
  W5-01 family; no version bump here). Migration: open the graph in Web Code
  UI, or add `--json` / `--machine` to existing `libra graph` /
  `libra agent graph` invocations.

### Added (plan-20260715 WIO-03, 2026-08-14, v0.19.158)

- **WIO-03 makes `ObjectReadBudget` object reads cancellable.** Inexact
  rename blob loads (status and diff) go through the out-of-process status
  I/O worker; a hung store read kills the helper within the remaining 5s
  batch window, maps the OID edge to a `metadata_*` warning, and continues
  other candidates. Per-object 2 MiB / total 64 MiB / 64 objects caps are
  unchanged. `docs/commands/status.md` and `docs/commands/diff.md` EN/zh
  record the real deadline.

### Added (plan-20260715 WIO-02, 2026-08-14, v0.19.157)

- **WIO-02 binds status/probe directory traversal to a repo-root
  fd/handle.** Linux uses `openat2(RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS)`
  with openat fallback; other Unix/Windows keep equivalent no-follow
  beneath walks. Escape and check→open directory-swap races map to
  `IoBlocked` without reading outside the worktree. Ignore sources under
  the tree are read through the same beneath path; schema/sort/
  `io_timeout` mapping is unchanged. Cached root fds are rewound before
  `fdopendir` so a prior listing cannot leave the next probe empty.
  Unreadable directories still emit the collapsed `dir/` marker with an
  absorbed `io_blocked` event. `docs/commands/status.md` EN/zh and
  plan-20260714 §B.3.2 record the fd/beneath semantics.

### Added (plan-20260715 WIO-01, 2026-08-13, v0.19.156)

- **WIO-01 moves status basic-scan/probe I/O into an out-of-process
  recyclable worker pool.** Cap 8 helpers, framed
  `Begin`/`Record`/`Checkpoint`, capability token, Unix process-group kill
  (plus Linux `PR_SET_PDEATHSIG` / parent-pid watchdog). A hung syscall
  recycles the worker, not the `status` caller; mid-stream kill keeps the
  last checkpointed partial and marks the edge `IoBlocked`. Rename content
  hashing and ignore lookups stay on the in-process thread pool (WIO-03 /
  SQLite `core.excludesFile`). `docs/commands/status.md` EN/zh and
  plan-20260714 §B.3.2 record the `io_timeout` semantics upgrade.

### Added (plan-20260715 W6-03, 2026-08-13, v0.19.155)

- **W6-03 uploads `install.sh` / `install.ps1` to the CDN root on tag.**
  `release.yml` adds `upload-install-scripts` (after the 4-target matrix):
  fail-closed tag/`Cargo.toml`/installer version gate, refuse older CDN
  copies, pin+checksum rclone, then `rclone copyto` the tagged scripts to
  bucket-root `install.sh`/`install.ps1` and verify CDN versions. Binary
  and Homebrew jobs are unchanged. Takes effect on the next `v*` tag.

### Added (plan-20260715 W4-05, 2026-08-13, v0.19.154)

- **W4-05 closes user-facing docs to Web-default.** `docs/commands/code.md`
  (+ zh-CN), `docs/development/tracing/code.md`, `COMPATIBILITY.md`,
  `docs/development/commands/_compatibility.md`, and `tests/INDEX.md`
  describe Web as the default, TUI/graph TUI as deprecated bake-window
  surfaces (hidden `LIBRA_CODE_LEGACY_TUI=1`; bare `--provider codex
  --resume` still uses the TUI resume driver), and must not claim
  physical deletion before W5-09. `threadGraph` / `GET /api/code/thread-graph`
  wire contract is documented in EN+zh-CN.

### Added (plan-20260715 W4-04, 2026-08-13, v0.19.153)

- **W4-04 moves the thread/version graph into Web Code UI.** Default
  `libra code` snapshots attach a capped indexed `threadGraph` (256 nodes,
  live heads preserved) and rehydrate via on-demand
  `GET /api/code/thread-graph?threadId=` (redacted, AbortController,
  on-demand with bounded 404/5xx retry plus lineage revalidation — not a
  timer poll). Interactive `libra graph` TUI prints a deprecation
  warning (removed in W5-08); `--json` / `--machine` stay. Hidden
  `LIBRA_CODE_LEGACY_TUI=1` is the hidden TUI rollback during the W4
  3-patch bake window (removed in W5-07). Bare `--provider codex --resume`
  still uses the legacy TUI resume driver (managed `--web-only --provider
  codex` continues to reject `--resume`). Final TUI closeout is W6-02
  after the W5-01 family breaking release (W5-09).

### Added (plan-20260715 W4-02, 2026-08-13, v0.19.143)

- **W4-02 makes `libra code --control stdio` the canonical JSON-RPC NDJSON
  automation client.** Shared `run_control_stdio_client` powers both the new
  path and legacy `code-control --stdio` (shim/docs closeout remains W4-09).
  Requires `--control-url` + `--control-token-file` until W4-10 discovery;
  conflict matrix separates MCP `--stdio` from control; single-dash
  `-control` fails closed; control URLs must be literal loopback IPs with
  proxy/redirect disabled so the process token cannot leave the machine.

### Added (plan-20260715 W4-01, 2026-08-12, v0.19.142)

- **W4-01 switches default `libra code` to the Web Code UI.** Bare
  launches print the Code UI URL + control info (no alternate-screen TUI).
  `--web` / `--web-only` remain deprecation-warning no-ops; hidden
  `LIBRA_CODE_LEGACY_TUI=1` restores the TUI for the bake window (removed in
  W5-07). Default browser-control is `loopback` with a per-session
  `X-Libra-Browser-Bootstrap` secret (`?bt=` on the printed URL; auto-open
  skipped so the secret never appears in opener argv). `--network-access
  allow` stays rejected on Web/MCP until the Plan gate owns per-execution
  sandbox network. Bare `--provider codex --resume` still uses the legacy
  TUI resume driver.

### Added (plan-20260715 W3-09, 2026-08-12, v0.19.141)

- **W3-09 migrates the built-in Code UI SPA to SSE wire v2.**
  `wrapClientForSseResilience` opens `GET /api/code/events?wire=2` and
  reconnects with the wire cursor (no full snapshot on reconnect). Backlog
  overflow (`event: resync` / `WIRE_V2_RESYNC_REQUIRED`) triggers one
  snapshot pull plus the W2-15 resync UI. DEFER-08 item 1 is recorded in
  `docs/commands/code.md`.

### Added (plan-20260715 W3-15, 2026-08-12, v0.19.140)

- **W3-15 adds Playwright real-browser e2e for Code UI.** `pnpm --dir web
  test:e2e` runs `web/e2e/**` against an already-started deterministic
  `--web-only` fake-provider runtime (`e2e/scripts/start-deterministic-runtime.sh`).
  Soft-skip when Chromium or `/api/health` is missing (local diagnosis only);
  `LIBRA_E2E_REQUIRE=1` / `CI=true` fail closed. Main chain covers submit →
  approval/user-input → goal/task/skill → usage → execution/repair →
  resume/cancel → SSE reconnect via user-visible DOM only. Startup, artifact
  paths (`web/test-results/`, `web/playwright-report/`), and cleanup are
  registered in `tests/INDEX.md` and `web/README.md`. Also restores missing
  `web/out/_next` static chunks so the embedded UI can load JS again.

### Added (plan-20260715 W3-07, 2026-08-12, v0.19.139)

- **W3-07 pins managed Codex interaction ownership.** App-server keeps the
  approval loop; Libra projects/forwards via sidecar `AgentRuntime` (no
  `pending_approvals`). Browser composer + cancel/approve paths are
  lease-on-first-write; user-input remains DEFER-07.

### Added (plan-20260715 W3-14, 2026-08-12, v0.19.138)

- **W3-14 gates large-session Code UI projection.** Hot window stays
  `MAX_CODE_UI_PROJECTION_*` (1,024 events / 8 MiB), named apart from W3-08
  transport backlog. JSONL resume walks records from the tail and stops at
  the cursor / window overflow, so a 10k-row session is not fully parsed
  for one-event fold. `large_session_projection_smoke` records
  runner/profile/sample size and asserts release p95 ≤ 5 ms.

### Added (plan-20260715 W3-08, 2026-08-12, v0.19.137)

- **W3-08 bounds Code UI SSE transport backlog.** Wire v2 uses
  `MAX_CODE_UI_TRANSPORT_BACKLOG_*` (1,024 events or 8 MiB, whichever first).
  Over-budget bootstrap or lagged catch-up emits `event: resync` /
  `WIRE_V2_RESYNC_REQUIRED` then ends the stream (no silent drop). Slow
  consumers share that policy; reconnect at `durableTail` continues without
  dup/loss. Live fan-out keeps an in-budget Event fast path and tip-only
  notifies when the tokio ring would exceed the byte cap.

### Added (plan-20260715 W3-06, 2026-08-12, v0.19.136)

- **W3-06 adds Code UI SSE wire v2 negotiation.** `GET /api/code/events`
  accepts `?wire=1|2` (or `Accept;libra-wire=`), defaults to v1, and fail-closes
  illegal versions. Wire v2 streams durable W1-06 workflow deltas with
  `cursor`/`eventId`/`kind`/`payload` and cursor reconnect; v1 full-snapshot
  remains the compatibility default until W3-09 (DEFER-08 checklist documented).

### Added (plan-20260715 W3-12, 2026-08-12, v0.19.135)

- **W3-12 closes Code UI sensitive projection redaction.** Session / SSE /
  diagnostics wire paths run through `SecretRedactor` + A0-08
  `env_name_is_forbidden` (env-file provider values registered as literals);
  empty rules fail closed (`REDACTION_FAILED`). Headless stream deltas are
  serialized so SSE transcript growth stays monotonic.
  `env_file_secrets_never_projected` pins ControlInfo schema and projection.

### Added (plan-20260715 W3-11, 2026-08-12)

- **W3-11 defines Code UI port/host posture.** Occupied listen ports fail
  closed with an actionable `--port` hint (no auto-scan), including the default
  TUI observe path. Non-loopback peers only get the static remote notice;
  snapshot/SSE/approval/API surfaces stay `LOOPBACK_REQUIRED`. Docs cover SSH
  port-forward remote access.

### Added (plan-20260715 W3-10, 2026-08-12)

- **W3-10 hardens Code control sidecar file posture.** `control.json` is
  written atomically (temp+rename) at Unix `0600` without a post-rename chmod
  symlink race; bare relative `--control-info-file` paths still resolve to the
  process cwd. Cross-worktree scope mismatch fail-closes
  (`code_control_info_scope_mismatch_rejected`); lease fence continues to reuse
  workspace lease fields rather than a second implementation.

### Added (plan-20260715 W3-13, 2026-08-12)

- **W3-13 opens Web launch flag parity for non-Codex providers.**
  `--web`/`--web-only` accept `--env-file`, `--context`, `--approval-policy`,
  and `--approval-ttl` with TUI-equivalent semantics (env-file still overrides
  process env/Vault). Managed `--provider codex` and MCP `--stdio` continue to
  fail-closed on surfaces they do not wire. Default `libra code` routing stays
  TUI until W4-01.

### Added (plan-20260715 W3-05, 2026-08-12)

- **W3-05 hardens Code UI write identity/request boundaries.** Browser
  attach/writes require a trusted loopback `Origin` (or same-origin Referer)
  derived from the bind address (including non-canonical loopback and port 80
  forms); automation keeps bearer/control-token auth without Origin.
  Per-session write rate limiting (`LIBRA_CODE_SESSION_WRITE_RATE_*`) returns
  `429 RATE_LIMITED` with `Retry-After`. Body limit remains fail-closed on both
  paths. Omitted attach `kind` with a control token resolves to automation so
  `libra code-control` stays compatible.

### Added (plan-20260715 W3-04, 2026-08-11)

- **W3-04 normalizes Codex websocket events into the shared `AgentEvent`
  envelope.** Managed `--web-only --provider codex` notifications map through
  `normalize_codex_notification` onto the same projection path as other
  providers; unknown methods become diagnosable `ProviderNotification`
  fallbacks. Terminal failed/cancelled turns persist `RunSnapshot` +
  `run_event` for restart recovery; ask-mode approvals still wait on browser
  `respond_interaction` until W3-07.

### Added (plan-20260715 W3-03, 2026-08-11)

- **W3-03 merges headless under `AgentRuntimeCodeUiAdapter`.** Production Web
  writes go through `web_admission` + the adapter; `HeadlessCodeRuntime` is
  lifecycle-only. Plain browser messages enter PlanPhase0 (risk gate, formal
  IntentSpec persist, IntentReview confirm/modify/cancel with TUI-parity revise
  mode); slash/`/` messages keep explicit direct turns. Resume reloads
  `intents/{id}.json` or fences; Modify persists `pending_revision.json`.

### Added (plan-20260715 W3-02, 2026-08-11)

- **W3-02 migrates Code UI harness to `--web-only` by default.** Remote
  matrices and scenarios drive HTTP/SSE against headless Web; workflow
  baselines move to `internal/ai/workflow_baseline`; web-only shutdown uses
  SIGTERM; plan/reclaim/MCP-TUI cases opt into `.with_pty_tui()`.

### Added (plan-20260715 W3-01, 2026-08-11)

- **W3-01 delivers `AgentRuntimeCodeUiAdapter`.** Production Code UI HTTP bridges
  usage/skills/resume through `AgentRuntimeHandle`; skill activate and in-process
  resume fail closed; `/usage` prefers thread scope and ThreadBundle bootstrap
  stamps durable `SessionState.id` (no longer mirrors thread UUID into sessionId).

### Added (plan-20260715 W2-09, 2026-08-11)

- **W2-09 delivers browser Goal/task/skill controls.** Domain helpers under
  `web/src/lib/code-ui/goal-task-skill/` drive `SessionGoalTaskSkill` panels for
  Goal start/status/cancel, task dispatch (active controller lease), and A0-07
  curated skill discovery validation (runtime skill HTTP remains W3-01).
  `BrowserController.withLease` is exposed for domain writes; command docs
  clarify browser-capable `/task/dispatch`.

### Added (plan-20260715 W2-08, 2026-08-11)

- **W2-08 delivers browser approval and `request_user_input` panels.** Domain
  helpers/fixtures under `web/src/lib/code-ui/interactions/` drive
  `ApprovalPanel` / `RequestUserInputForm` / `SessionInteractions`. Headless
  projection now emits `header` / `isOther` / `isSecret` / option descriptions,
  filters blank/duplicate option labels, and is covered by a Rust wire unit
  test. Managed Codex sessions keep cancel but disable browser respond.

### Added (plan-20260715 W2-07, 2026-08-11)

- **W2-07 delivers the Web Code UI wire adapter foundation.** Shared
  TypeScript types, HTTP/SSE client (with loopback wire smoke), session store,
  browser controller lease (stable `sessionStorage` client id), phases,
  view-model helpers, and Vitest fixtures land under `web/src/lib/code-ui/`.
  The shipped page is a foundation-only placeholder; domain panels remain
  W2-08+. CI `compat-web-check` runs `pnpm --dir web test`, and Next
  `generateBuildId` is pinned so committed `web/out/` stays reproducible.

### Added (plan-20260715 W2-12, 2026-08-11)

- **W2-12 delivers UI-neutral runtime usage attribution and query.**
  `RuntimeUsageService` attributes spend at repo/session/turn/sub-agent,
  reuses `internal/ai/usage` with session-scoped event idempotency, and
  exposes Known/Partial/Unknown (plus mixed `$exact + ~$estimate`) on CLI,
  CSV/JSON, and TUI formatters. Headless and TUI bind durable turn IDs;
  cancel races prefer completed provider usage over placeholders.

### Added (plan-20260715 W2-11, 2026-08-11)

- **W2-11 migrates plan-execution failure classification and repair loops into
  the runtime.** Runtime-owned repair state classifies plan, IntentSpec, and
  manual-action failures; persists Continue/Cancel gates for crash recovery;
  projects bounded redacted failure evidence to Code UI; and requires a higher
  `maxAttempts` to extend an exhausted automatic-repair limit. English and
  zh-CN Code/control documentation describe the wire contract.

### Added (plan-20260729 CT3-02, 2026-08-10)

- **First clean-room wave of upstream Git compatibility tests (t4).** 77
  scenarios from the upstream `t4*` diff family, rewritten against Libra's own
  helpers with expectations taken from the Git-side contract, land as
  `tests/command/t4_port_test.rs` (`command::t4_port_test::*`). Each test maps
  to exactly one row in the evidence ledger under `tests/compat-ledger/t4/`,
  which records the command, the surface it proves, and the doc anchor that
  documents it; `PROVENANCE.md` additionally records, per scenario, a replayable
  three-part source for the expected value and the output Libra actually
  produced. No upstream text is vendored — a clean-room gate rejects any shared
  8-token window or 40-character common substring with the pinned corpus.

### Fixed (plan-20260729 CTF-P01/CTF-P03, 2026-08-10)

- **`diff --check --exit-code` lost the "there is a difference" status bit.**
  Git adds the two contributions — 1 for a difference, 2 for whitespace damage —
  so a clean difference is `1` and a damaged one is `3`. Libra returned `0` and
  `2`. Migrating the upstream scenarios surfaced it.
- **`update-index --add --remove` silently staged nothing.** Given both flags on
  a path that exists, the command exited 0 without staging it. The two are
  non-exclusive permissions in Git: presence on disk decides per path.

### Added (plan-20260715 W2-06, 2026-08-10)

- **W2-06 migrate Goal / task.dispatch / skill consumer onto runtime control.**
  `ExecutionControlService` owns durable Goal start/status/cancel (JSONL resume
  of the latest Created goal), headless overrides Goal/task adapter defaults,
  `task.dispatch` shares the SubAgentDispatcher gate with file-history batches,
  and Code skill search/activation consumes A0-07 only
  (`skill_search_activation_uses_a0_projection`).

### Added (plan-20260715 W2-05, 2026-08-10)

- **W2-05 unify approval and `request_user_input` on runtime interaction state.**
  TUI/headless register `AwaitingToolApproval`/`AwaitingUserInput` via
  `register_interaction_with_delivery`; idle/no-turn paths fail closed; managed
  Codex denies without a runtime turn. Multi-question validation + cancel
  drop continuations; sequential resolutions persist as a durable batch.
  Contract: `request_user_input_multi_question_and_cancel_fail_closed`,
  `interaction_pending_owner_is_runtime_only`,
  `sequential_user_input_resolutions_all_persist_on_terminal_success`.

### Added (plan-20260715 W2-04, 2026-08-10)

- **W2-04 migrate confirmed plan execution onto the runtime serialized queue.**
  `submit_confirmed_plan_execution` + `DeferredPlanExecutionExecutor` own
  Orchestrator::run after network Allow/Deny; TUI uses a three-state crash
  handoff (network → submitting → admitted), turn-scoped observe completion,
  JoinError → indeterminate, and keeps managed Code UI off the local runtime.
  Contract/handoff coverage: `plan_execution_enters_runtime_queue`,
  `plan_execution_handoff_tests`. Repair loops remain W2-11.

### Added (plan-20260715 W2-03, 2026-08-10)

- **W2-03 migrate Phase 1 Plan review + network policy into runtime.**
  Runtime owns Execute→network gates via durable workflow markers,
  `CompletedHoldQueued` / `hold_queued_admission`, and AckDeliveries;
  TUI parks/settles/restores Plan review and network policy, persists an
  execution handoff before Allow/Deny, and clears it durably before execute.
  PTY coverage: `plan_review_network_policy_survives_resume_and_can_be_denied`.

### Added (plan-20260715 W2-02, 2026-08-09)

- **W2-02 migrate Phase 0 IntentSpec review into runtime interaction state.**
  Runtime owns confirm/revise/cancel via `IntentReviewAckDelivery` with durable
  `IntentReviewRequested`/resolution, crash-resume reopens the gate, and
  mutating turns stay fenced until confirm. Marker-persistence failure leaves
  Phase 0 Pending (fail-closed) instead of terminalizing success.

### Added (plan-20260715 W1-08, 2026-08-09)

- **W1-08 structured process lifecycle shutdown for `libra code`.**
  Introduce `LifecycleShutdownOwner` so SIGINT/SIGTERM, bootstrap failure, and
  normal exit share one deadline/result contract for runtime, listeners,
  managed Codex child, controller lease, FUSE sweep, and control files.
  Document SIGTERM graceful shutdown; add web-only and TUI SIGTERM regressions.

### Changed (plan-20260715 W0-03, 2026-08-09)

- **W0-03 Web-only completion gate is machine-checkable.**
  Strengthen `code_web_only_completion_gate` to assert AC1–AC6 checklist
  items (parity gates, direct-turn non-completion, A0 inputs, non-Code TUI
  consumers, product decisions) and fail with a missing-item list; document
  that current Web-only direct-turn is not a completion state.

### Changed (plan-20260715 W0-02, 2026-08-09)

- **W0-02 freeze TUI-owned workflow baselines before Web migration.**
  Add `workflow_baseline` choice/threshold contracts, named
  `plan_workflow`/`plan_review`/`repair`/`user_input`/`goal_task` filters, and
  an INDEX inventory of baseline test names + expected outputs so later Web
  harness work can retarget these behaviors instead of deleting them.

### Changed (plan-20260715 W0-01, 2026-08-09)

- **W0-01 source-anchor refresh for the Web-only migration plan.**
  Revalidated `docs/development/tracing/code.md` C1–C10 conflict table, A0-02..A0-11
  bind/consume inventory, runtime namespace decision, and non-Code TUI consumers
  against `main` @ `25c8f6a` / v0.19.104; synchronized plan fact-baseline anchors and
  recorded `agent/graph.rs` as a required W5-03/W5-10 consumer. Docs-only; no runtime
  behavior change.

### Added (plan-20260729 CT2-03, 2026-08-08)

- **The clean-room phrase allowlist, frozen.**
  `tests/compat-ledger/PHRASE_ALLOWLIST.txt` is the single escape valve from
  the zero-overlap clean-room contract, and it ships empty on purpose: nothing
  has been migrated yet, so there is no unavoidable overlap to approve. Its
  sha256 sidecar freezes it, and `phrase_allowlist_sidecar_matches` checks the
  digest along with the entry format — at most 20 lines, no blank lines, and
  every entry exactly 8 tokens, because the gate's window is 8 tokens wide and
  an entry of any other length would sit there looking approved while matching
  nothing.

### Added (plan-20260729 CT2-02, 2026-08-08)

- **The surface lock and its generator.**
  `tests/compat-ledger/SURFACES.gen` turns the authoritative surface registry
  (`docs/development/gap/surface-registry.tsv`, GC-13) into a deterministic,
  sorted lock, refusing any row whose evidence anchor does not resolve to text
  that mentions both the command and the surface — command documentation is
  free text, so a per-flag status can only ever be asserted and anchored, never
  scraped. `compat_ledger_schema` now adjudicates each ledger row's
  `surface_compatibility` against that lock using GC-13's longest-exact-prefix
  rule, and a `direct` row must have the lock, not just the row, saying
  `git-compatible`.

### Added (plan-20260729 CT2-01, 2026-08-08)

- **The compatibility evidence ledger and its guard.**
  `tests/compat-ledger/` records one row per migrated upstream Git test
  scenario, and `compat_ledger_schema` enforces ADR-CT-03's field set on every
  row. The rules that make it evidence rather than paperwork: the command's
  compatibility tier is recomputed from `COMPATIBILITY.md` instead of being
  read from the row, `surface_evidence` must resolve to text that actually
  mentions both the command and the surface, and the field set is closed. An
  empty ledger passes, so the tree can land before the evidence does.

### Added (plan-20260729 CT1-03, 2026-08-08)

- **`update-ref`'s `<oldvalue>` accepts revision expressions too**, through the
  same entry point as `<newvalue>` — so `libra update-ref -d refs/heads/topic
  HEAD` guards a delete without a manual `rev-parse`. It is deliberately not
  type-checked: `<oldvalue>` asserts what the ref points at now, so the
  resolved id is compared verbatim and naming an annotated tag there is an
  ordinary compare-and-swap mismatch, matching Git. The all-zero "must not
  exist" spelling is unchanged, and branch protect/archive enforcement stays
  inside the same transaction and fail-closed on a metadata read error.

### Added (plan-20260729 CT1-02, 2026-08-08)

- **`libra update-ref refs/heads/<branch> <newvalue>` accepts revision
  expressions.** The value operand previously took a full object id only, so
  `update-ref refs/heads/x HEAD` failed. It now goes through the shared
  resolver — branches, tags, `HEAD`, `HEAD^`/`HEAD~n`, abbreviated ids and
  `<rev>^{commit}` all work — with no implicit peel: what the expression names
  must itself be a commit, so a lightweight tag is accepted and a bare
  annotated tag is refused naming the type that was resolved, exactly as Git
  does. Unresolvable or non-commit values are `LBR-CLI-003`; the `ref:` and
  null-id syntax refusals stay `LBR-CLI-002`; a failure inside the object
  store keeps its repository/IO code instead of being reported as bad input.
  `<oldvalue>` is unchanged.

### Fixed (plan-20260729 CT1-01, 2026-08-08)

- **`libra config <key>` reads an encrypted ordinary key instead of refusing
  it.** A stored encrypted value used to be read as assignment intent, so a
  bare read of an ordinary key reported "missing value for protected key"
  with exit 2. That inference is now drawn only when the caller spells out
  the assignment (`config set <key>` / `--add`); the bare form renders
  `<REDACTED>` through the same `reveal=false` path as `config get`.
  Protected keys keep their interactive secure-assignment path — an
  intentional divergence from Git, now registered on the `config` row of
  `COMPATIBILITY.md` and documented in both language editions of
  `docs/commands/config.md`.
- **`libra config -z <key>` terminates with NUL like `config get` does.** The
  bare read hardcoded newline termination, so the two spellings of the same
  read disagreed byte for byte whenever `-z`/`--null` was in play.

### Fixed (plan-20260714 W4 cross-review, 2026-08-08)

- **`cloud restore` no longer overwrites locally scoped agent sessions.**
  Cloud content carries no workspace ownership, so the restore upsert now
  skips rows a live workspace owns and reports how many it left untouched.
- **`agent session stop/resume` respect capture ownership**: rows another
  worktree scope owns are refused (naming the owner), and `legacy_unknown`
  rows stay excluded from every new write until explicitly adopted.
- **The remove/prune agent-lease gates fail closed on store errors and
  ignore expired leases** — "let it expire" in the refusal hint is now
  true, and a crashed agent can no longer block a human's `worktree
  remove` forever. Both directions are pinned by
  `remove_is_lease_gated_and_expiry_unblocks`.
- **Doctor accuracy**: a released workspace no longer reports its retained
  provenance owner as a "held" lease; the by-id form treats a repository
  that never ran the workspace migration as "no such workspace" instead of
  scope corruption; and a new `capture_rows_stale_fence` finding makes
  capture rows bound to a dead workspace generation diagnosable.
- **`worktree list --schema-version`** exists per §C.8: the data half now
  names its own `schema_version` (2 — the shipped shape), and requesting
  the never-shipped v1 is refused with LBR-CLI-002 rather than answered
  with a fabricated shape. The duplicated/contradictory doctor and repair
  doc sections (EN/zh/COMPATIBILITY/help EXAMPLES) are merged and
  corrected.
- **§C.12**: `task_worktree_orphan_recovered_without_raw_payload` now
  exists — a crashed owner's task workspace is swept, recovered, and
  diagnosed with the no-raw-payload guarantee proven against the live
  schema.

### Fixed (plan-20260714 W3 cross-review, 2026-08-08)

- **`worktree remove` and `prune` now refuse a worktree an agent holds a
  live workspace lease on** (§C.7): remove checks by path and by worktree
  id; prune skips leased entries by worktree id (a missing path can never
  match a canonicalizing path query).
- **Crash-journal recovery no longer guesses.** An unknown intent op is
  kept pending instead of deleted (it is a NEWER binary's crash anchor);
  interrupted-move recovery advances only when the directory's own gitdir
  identity matches the journal (a stranger's directory can no longer be
  renamed or adopted); a crashed re-attach lifts the frozen marker only
  from a directory that still carries the entry's identity; a crashed
  keep-dir detach is not rolled forward over sequencer work started after
  the crash; and `reconcile_lifecycle` restores a detached marker only into
  an identity-matching directory.
- **Two migrate-layout crash points are no longer unrecoverable.** The
  migrate-marker is written FIRST in the prepared gitdir, and recovery
  rolls back a marker-less prepared dir whose name binds it to the exact
  pending journal and whose contents are at most the engine's three files.
  Backup-symlink removal failures now keep the journal instead of leaking
  the link silently.
- **`worktree repair <path>` restores a DANGLING commondir pointer**
  (previously misclassified as "points at a different storage" and refused
  forever), still refuses an EXISTING different storage, and refuses
  tombstoned entries outright. `--resolve-identity` now writes the SQL
  lifecycle mirror row the 2026072402 down-migration guard reads, and
  doctor no longer reports the legitimate post-resolve Active+Detached pair
  as a collision. `add`'s handled failures resolve their intent-journal
  row; `remove --delete-dir` treats a real parent-fsync failure as
  non-durable (tombstone kept); stale corrupt-identity and v3-rollback
  hints now name actions that exist.

### Fixed (plan-20260714 W2 cross-review, 2026-08-07)

- **gc run from a linked worktree no longer prunes blobs staged only in
  main.** The index root collector seeded the INVOKING worktree's index and
  then skipped the registry's main entry — correct when gc runs from main,
  and a silent data-loss hole when it runs from a linked worktree: main's
  private index was simply absent from the root set. Main's index is now
  seeded explicitly from the common storage root, and a two-phase regression
  (quarantine pass + aged prune pass, invoked from the linked worktree)
  pins it.
- **A crashed `stash branch` can no longer lose the detached commit it must
  restore.** Its rollback journal named two OIDs but was classified as a
  GC non-root on the claim that both were anchored elsewhere — untrue for
  `prior_detached`: the HEAD switch rewrites the reference row without a
  reflog entry, so a worktree that was detached loses that commit's last
  anchor the moment the switch commits, and recovery would re-point HEAD at
  a pruned object. The journal is now a traced sidecar root for its short
  life (corrupt JSON fails the prune closed, like every sidecar).

### Added (plan-20260714 W2 cross-review, 2026-08-07)

- **The `WorktreePseudoRefs` per-worktree isolation is now pinned by the
  §C.12 named regression `linked_pseudo_refs_resolve_per_worktree`.** The
  service projects `ORIG_HEAD`/`MERGE_HEAD`/`CHERRY_PICK_HEAD`/
  `REVERT_HEAD`/`REBASE_HEAD`/`FETCH_HEAD` from each worktree's own scoped
  state; exposing them as `rev-parse` names stays DEFERRED by §C.5's
  explicit contract (`COMPATIBILITY.md` rev-parse row), and the compat
  guard continues to pin that refusal.
- **The §C.4.3 GC inventory is now typed, exhaustive and guarded.** Every
  source — 38 SQLite columns and 14 file surfaces, one struct — declares
  its storage kind, schema/version, read bound, corruption policy and root
  type, with a structural guard forbidding inconsistent combinations (a
  keep-alive root must fail closed or defer; an index-only source must be
  lenient). The file half gained the rows the walk already collected but
  never named (every worktree's private index, the shared stash reflog,
  `refs/replace`) and explicit classifications for every W1/W2 sidecar. A
  forward guard scans production sources for every file name joined onto a
  storage root and fails when one is neither inventoried nor excluded with
  a reason — the exclusion list is checked for staleness and forbidden from
  shadowing an inventory row.
- **GC roots now fail closed on missing objects, from typed schemas.** Each
  in-progress sidecar's OWNER exposes a typed extractor for its semantic
  OID fields (merge/revert state, rebase-aux — including the rewrite map's
  keys — and the stash-branch journal): a field naming a missing object
  refuses the prune naming the file and field, while text fields that
  merely look like hashes (branch names, conflict paths) are never
  extracted and can no longer block GC. Private-index entries are probed
  the same way (gitlinks exempt — a submodule commit is legitimately
  absent).
- **The whole GC root collection binds to ONE storage root**, resolved from
  the request pin at entry and threaded through every file-backed collector
  (indexes, registry, sidecars, `refs/replace`) — an in-process working-
  directory move can no longer pair one repository's indexes with
  another's registry.
- **`CHERRY_PICK_HEAD` now means what §C.5 says.** The sequencer row is
  persisted before every pick attempt, so the row alone cannot distinguish
  a conflict stop from a hard-error stop; a durable conflict-phase flag in
  the persisted options now discriminates — set on every conflict stop,
  stripped on resume — and the pseudo-ref projection requires it (rows from
  older binaries project nothing; `ORIG_HEAD` stays defined for any
  sequence).
- **`WorktreePseudoRefs::for_request()` binds its file projections to the
  request pin**, not the ambient working directory, so an in-process
  directory move cannot pair one repository's database rows with another's
  merge/revert/`FETCH_HEAD` files.

### Fixed (plan-20260714 W1 cross-review, 2026-08-07)

- **`libra service` no longer wedges when two dirty-mark requests arrive at
  once.** The handler took the worktree registry lock — a BLOCKING `flock` —
  inline on a runtime worker. sqlx returns a pooled connection by *spawning*
  a task, and a spawn from inside a poll lands in that worker's
  non-stealable LIFO slot, so blocking the worker stranded the
  connection-return; because sea-orm pins SQLite pools to a single
  connection, the request that held the lock then waited out the full sqlx
  acquire timeout for a connection that could never come back. Both requests
  stalled ~60s and the client timed out. The lock is now taken via
  `spawn_blocking`, so concurrent marks complete in milliseconds. Pinned by
  the previously-failing `linked_service_dirty_mark_is_scoped` and by a new
  deterministic regression that holds the registry lock from outside the
  process.
- **A service database failure is no longer reported as a repository
  mismatch.** `validate_request_repository` collapsed a failed identity read
  into an empty string, answering `409 this service serves repository ''` —
  a confident wrong verdict for what is really a server error. It now
  returns 500 with the underlying error, which is what made the deadlock
  above unreadable in the first place.

### Added (plan-20260714 W1 cross-review, 2026-08-07)

- **The §C.4.1.1 mutable-state ownership registry**
  (`internal::mutable_state_ownership`) declares every mutable table's
  `Repository | Worktree | Composite` ownership with its rationale, and its
  guards check those declarations against the real schema from two
  independent directions. Structurally: a table carrying a `worktree_id`
  scope column must be declared, and a scoped declaration must be backed by
  a real column. Exhaustively: every table a freshly created repository
  database contains (read back from `sqlite_schema`) must be classified,
  and so must every table created by DDL anywhere in the tree — SQL files,
  `CREATE TABLE` text in Rust, and sea-orm `create_table_from_entity` calls,
  found by walking the syn AST with `#[cfg(test)]` items evaluated out. A
  new per-worktree table can no longer ship unregistered.
- **`libra service run` refuses to reclaim another repository's control
  file.** Startup now resolves its own `ControlScope` and passes the
  repository-policy takeover gate before writing the token or the info file,
  and stamps its own record with the repository identity. A legacy
  (pre-stamping) record is still adopted when its `workingDir` resolves to
  this repository's storage, so upgrading over a killed service keeps
  working.

### Fixed (plan-20260714 W0 cross-review, 2026-08-06)

- **`.libra/info/exclude` and `.libra/info/attributes` now actually work —
  and are per-worktree.** The info-file resolver only recognized a literal
  `.git` marker, so in a pure `.libra` repository the `info/exclude` that
  `libra init` itself creates was never consulted by the ignore engine
  (`status`, `add`, `clean`, `check-ignore`, `ls-files`) and
  `info/attributes` was never consulted by the attributes engine
  (`check-attr`, LFS classification, diff textconv). Resolution now targets
  the CURRENT worktree's own gitdir — `.libra` plus, in a dual-layout tree
  converted from Git, the local `.git` (both consulted, so existing
  `.git/info/*` rules keep working) — and no longer follows `commondir`:
  each isolated linked worktree reads its own `info/*` view (plan-20260714
  §C.4.1.1; intentionally different from Git's repository-wide sharing —
  share rules with a root `.libraignore`/`.gitignore` instead). Docs and
  COMPATIBILITY rows updated in both languages.
- **The publish worker-template manifest is one per repository again.**
  `publish init`/`status`/`deploy`/`unpublish` derived
  `.libra/publish/worker-template-manifest.json` from the invoking
  worktree's root, so a linked worktree read and wrote its OWN manifest
  under its local gitdir — forking the deploy drift gate per worktree. The
  path now resolves through the fail-closed common-storage resolver
  (corrupt `commondir` refuses rather than minting a per-worktree
  manifest). The reported `manifest_path` stays worktree-relative for
  main-worktree invocations (byte-identical to the previous output) and is
  an absolute path from a linked worktree — the old relative string would
  have named a file that does not exist there.
- **Two cross-worktree process caches are gone.** The `core.ignorecase`
  workdir probe no longer latches a single process-global verdict (a
  multi-worktree host process handed worktree A's filesystem answer to
  worktree B): it re-probes this invocation's worktree, honoring a request
  pin. The `refs/replace` map is no longer cached at all — resolution
  follows the chain by reading `refs/replace/<oid>` by name, so the common
  no-replacement case costs one failed open and EVERY mutation (this
  process's `replace` create/delete, or another process's, including a
  same-name `-f` retarget) is visible to the next resolve; a long-lived
  host can no longer serve stale replacements or another repository's map.
  The revision-ordinal builder pins one snapshot across its signature stamp
  and chain walk, so the stamped digest always describes the map the walk
  used (plan-20260714 §C.4.1.1 process-cache rules).
- **Corrupt worktree metadata fails closed instead of masquerading as
  absent.** A `commondir` pointer whose target is missing, is not the
  repository's terminal common storage (a self-pointer, a pointer chain, a
  pointer at another worktree's gitdir), or cannot be inspected now
  refuses with the `worktree repair` remedy — previously the resolver
  returned the unresolvable path as-is, so every downstream lookup
  (database, objects, sandbox policy) saw "not found" where "corrupt
  worktree" was the truth, and sandbox policy in particular degraded to
  permissive defaults. Lifecycle markers (`commondir`,
  `detached_from_registry`, `migrate-marker`) are judged without following
  symlinks and only a definitive "no such file" clears them, so a dangling
  or unreadable marker can no longer unfreeze a frozen worktree or let
  repository discovery climb into the main worktree's scope. A
  sandbox-policy resolution failure inside a repository is likewise an
  error rather than a silent fall-back to defaults (outside any repository
  the default config is still legitimate).

### Added (plan-20260714 W0 cross-review, 2026-08-06)

- **Linked-worktree guards for the Code/Agent runtime surfaces.** Until the
  unified Code/Agent config resolver lands (plan-20260715 W4-06..W4-08),
  Code/Agent configuration reads are split-brained in a linked worktree
  (most read the local gitdir, sandbox reads common storage). `libra code`
  (all modes) and every `libra automation` subcommand now fail closed
  there with a hint to run from the main worktree — gating the SESSION's
  working directory, so `libra code --cwd <linked-wt>` from main is refused
  too — and automation dispatch (VCS events and hook-lifecycle ingestion
  alike) is disabled with exactly one warning per command instead of
  silently no-opping against an empty rule set. The §C.4.1.1 ownership
  inventory (`internal::config_ownership::CODE_AGENT_CONFIG_OWNERSHIP`)
  registers every Code/Agent config/approval surface with source-scan and
  structural guards that fail on unregistered or rotten rows.
- **`worktree doctor` handles the legacy common info files, and
  `worktree add` probes the target filesystem.** With linked worktrees
  present, the read-only doctor report states that common `.libra/info/*`
  applies only to the main worktree and names the two new explicit,
  confirmed, audited actions: `--adopt-info-to <worktree-path>` (copy into
  ONE linked worktree's own gitdir, never overwriting its files) and
  `--clear-common-info` (delete from common storage). `worktree add` now
  probes the target volume's case behavior and warns when it disagrees
  with the repository's persisted `core.ignorecase` (probed from main at
  init) — the persisted per-worktree config overlay rides the W4 unified
  resolver per the dated plan revision.

### Fixed (plan-20260714 R0-8 cross-review, 2026-08-06)

- **The racy-index window can no longer make `status` fabricate "clean" for
  a modified file.** Every index writer used to stat the file only AFTER
  reading and hashing it, so an edit landing in that window paired a
  post-edit stat with a pre-edit hash — plain `status` then trusted the
  stat shortcut and hid the modification (observed in this repository with
  a concurrently-edited CHANGELOG.md; `commit` would also have built a
  stale tree from the poisoned entry). Three layers now close it:
  1. writers stat BEFORE reading and a shared `verified_index_entry`
     helper smudges the entry (zeroed volatile stats → always
     content-compared) whenever the pre/post stats disagree — applied to
     `add` (new/modified/renormalize), `commit` auto-stage,
     `update-index`, and `mv`;
  2. `am`'s mode restage, `add --chmod`, and `stash` restore, which pair
     an inherited hash with a fresh stat without reading content, now
     always smudge;
  3. `status` and `diff` additionally trust a matching stat only for
     files strictly OLDER than the index snapshot itself (Git's
     racily-clean rule), and `status` uses the same guarded size
     comparison as `diff` (a >4GiB stat can no longer collide with a
     truncated 32-bit size).
  Pinned by `racy_edit_between_read_and_stat_smudges_the_entry` and
  `racily_clean_entry_is_content_compared_not_trusted`; `diff` also stats
  the index snapshot once per batch instead of once per candidate.

### Changed (plan-20260714 R0-7 cross-review, 2026-08-06)

- **The status warning enums are now self-registering.** `StatusWarningCode`
  and `StatusWarningSource` are declared through a macro that generates
  their `ALL` registries from the single variant list, so a new variant can
  no longer ship while absent from the registry the schema snapshot and doc
  guards walk. `StatusWarningSource::ALL` is new public API; numeric
  discriminants of both enums are explicitly pinned to their original
  values (and now regression-tested variant by variant), and serialized
  wire names are unchanged.
- The `json_io_blocked_rename_branch_schema` regression no longer accepts a
  `null` rename association for its object-store-backed fixture, closing a
  vacuous pass.

### Fixed (plan-20260714 R0-6 cross-review, 2026-08-06)

- **Rename-aware short and porcelain v1 rows keep the source's staged
  state.** An unstaged rename whose source carried a staged change printed
  a space-staged ` R old -> new`, erasing the `M`/`A` component porcelain
  v2 derives; the shared `ShortStatusEntry` builder now derives it the same
  way (`MR`/`AR`). Pinned by `short_rename_keeps_staged_source_component`.
- The `io_blocked[].rename` JSON object now escapes its `from`/`to` paths
  losslessly like every other JSON path field (was `display()`-lossy for
  non-UTF-8 pairs — latent, since staged pairs currently require UTF-8
  index paths).

### Fixed (plan-20260714 R0-5 cross-review, 2026-08-06)

- **Porcelain v2 non-rename rows keep their real metadata from a
  subdirectory.** The `1` and `u` row writers re-projected the already
  repository-root-relative payload through the current directory, so from a
  subdirectory every index/HEAD lookup missed and the mode/hash columns
  silently fell back to fabricated `100644`/all-zero values (the printed
  path stayed correct, which hid it). The writer now uses the projected
  payload directly. Pinned by
  `porcelain_v2_non_rename_rows_from_subdirectory_keep_real_metadata`.
- **Porcelain v2 spells an unmodified side as `.` like Git.** `1` rows
  printed the v1-style space (`1  M`) instead of Git's `1 .M`, breaking
  fixed-column consumers; the XY columns now emit `.` for the clean side.
- Documented the `MR`/`AR` refinement (an unstaged rename whose source
  carries a staged change takes real HEAD-tree columns, not the `.R` copy
  fixup) in the porcelain v2 docs, EN + zh.
- **A staged deletion's `1 D.` row spells its absent sides `000000`** (index
  and worktree) with the zero index hash, like Git — the old fallback
  fabricated `100644` for lookups that cannot succeed; any OTHER missing
  stage-0 entry now fails closed instead of forging metadata, and an
  unreadable worktree mode on `1` rows is a hard error like the rename arm.
- **An unresolved conflict never leaks out of its `u` record.** The
  stage-0-less index classifies a conflict as staged-deleted, which let it
  (a) emit a bogus duplicate `1 D.` row and (b) pair as a staged-rename
  SOURCE with a same-content staged addition, producing a `2` record for
  an unresolved path. Conflicts are now excluded from rename pairing (with
  classification restored after detection) and from the ordinary v2 row
  loop. Pinned by `unmerged_path_never_pairs_as_a_staged_rename_source`
  and duplicate-count assertions in both `u`-row tests.

### Fixed (plan-20260714 R0-4 cross-review, 2026-08-06)

- **`--find-renames` raw scores parse exactly like Git.** The score parser
  is now a faithful port of Git's `parse_num`: precision stops at five
  significant digits — `0.000019` is score 0 and therefore the 50% default,
  never a near-zero threshold that would pair almost anything — and the
  digitless zero forms (``, `%`, `.`, `.%`) parse to the default like Git
  instead of erroring. Pinned by `rename_score_matches_git_parse_num`.
- **The status API percent field rejects out-of-range values.**
  `find_renames: Option<u8>` silently clamped 101..=255 to exact-only; the
  clap parser path (value range 0..=100) and the resolver (struct-literal
  path, `LBR-CLI-002`) both fail closed now. Pinned by
  `api_percent_above_100_fails_closed`.
- **`status -- --null` proves pathspec propagation.** The spec-named
  `null_after_separator_ignored` regression now exists (it was a phantom
  name — the file named `--null` must match its own pathspec while the
  modified sibling is excluded); several stale §B.4.3/§B.9 test names were
  synced to the real manifest names.

### Fixed (plan-20260714 R0-3 cross-review, 2026-08-05)

- **A directory listing that breaks part-way no longer claims a complete
  scan.** The untracked walk collected `ReadDir` entries through a nested
  result whose INNER error (a mid-iteration `ReadDir::next` failure — the
  errno-passthrough class seen on NFS/FUSE) was discarded: entries after
  the failure were silently dropped, no `io_blocked` event was recorded,
  and `--scan` could replace the dirty cache with the partial snapshot.
  The listing now flattens both error layers into the io_blocked recorder;
  a genuine `TimedOut` maps to the `io_timeout` reason (the old sentinel
  arm swallowed it), and a single entry vanishing mid-iteration is skipped
  without abandoning the directory. Pinned by
  `main_scan_mid_iteration_readdir_error_reports_partial` (three legs,
  seam-injected error kinds).
- **The spec-mandated `main_scan_ioblocked_keeps_ignored_json` regression
  now exists** (ignored entries collected before an EACCES-blocked sibling
  survive the JSON partial) — the plan named it but it was never written;
  two stale marker-test names in the plan were synced to the real manifest
  names.

### Fixed (plan-20260714 R0-2 cross-review, 2026-08-05)

- **The dirty-cache read-pause test seam is no longer compiled into release
  binaries.** `LIBRA_TEST_CACHE_READ_PAUSE_MS` was runtime-gated on
  `LIBRA_TEST` but present in release builds, unlike the rest of the seam
  family; it is now `#[cfg(debug_assertions)]`.
- **The comparison-budget test seam is tighten-only.** A gated
  `LIBRA_TEST_STATUS_COMPARISON_BUDGET` above the production cap now clamps
  to the cap instead of raising it; pinned by a cap+1 regression. Also
  pinned: the rename-candidate NotFound degradation keeps the destination's
  untracked row, and the three R0-1 delay seams are inert without the
  harness gate.

### Fixed (plan-20260714 R0-1 cross-review, 2026-08-05)

- **A symlink planted at `refs/stash` or `logs/refs/stash` now fails closed
  instead of being silently "repaired" away.** `reconcile_stash_ref` read the
  stash tip through a symlink-following `read_to_string` and, when the log
  was absent, deleted whatever sat at the ref path — destroying the planted
  symlink and letting `status`/`commit` report a clean stash where the
  pinned contract demands `LBR-IO-001` (exit 128). Both paths now get
  no-follow guards before any read or repair; repair remains only for
  regular-file states the stash writer can actually produce.
- **The rename-detection batch deadline now bounds the SEQUENCE of I/O
  operations, not each operation from a stale entry window.** Both worktree
  readers recompute the remaining window immediately before every
  stat/readlink/read — including the LFS `.gitattributes` classification,
  which previously ran outside any deadline and could hang the command on
  a blocked attributes file — and the preliminary snapshot stats in
  `status` run under the same shared batch window instead of their own
  standalone 10s allowance, so a 5s batch can no longer run to a multiple
  of itself. Pinned by `sequential_ops_share_the_batch_deadline` and the
  two `lfs_classification_is_bounded_*` regressions.
- **`libra status` and `libra diff` parse a padded integer config value
  identically.** `status` handed the untrimmed value to the
  pre-trimmed-only integer parser, so a stored `diff.renameLimit` of
  `" 5 "` worked in `diff` but failed closed as a usage error in `status`.
  Pinned by `padded_rename_limit_value_parses_in_both_commands`.
- **The `similarity_budget_exceeded` warning names its survivors.** The
  message claimed the whole inexact pass was discarded; only the exhaustive
  stage is — exact and already-scored unique-basename matches are kept.
  Message, EN/zh docs, and full-text pins updated.
- **An over-cap symlink read charges the worktree budget.** The refusal
  path returned `WorktreeTooLarge` without charging the readlink bytes
  already pulled from the OS. Pinned by
  `symlink_refusal_charges_the_worktree_budget`.

### Fixed (plan-20260714 W0 implementation review)

- **An unconfirmed `worktree repair` no longer upgrades the repository schema
  on its way to refusing.** The confirmation refusal is documented as
  byte-for-byte side-effect free, but the command used to apply pending
  database migrations (an irreversible write) in its preflight before the
  `--confirm` check ran. A would-be-refused invocation now skips every
  migration-applying database open — the CLI preflight and both dispatch
  entry points (legacy and FUSE) route it straight to the pure refusal.
  Pinned by `unconfirmed_repair_applies_no_pending_migrations`, which drops
  the newest applied-migration row and proves the refusal leaves it dropped.
- **`worktree doctor` is now strictly read-only on a pre-upgrade
  repository.** The scope report used to reach the shared, migration-applying
  connection for one lookup, so the one command documented as safe to run
  before you decide to upgrade could itself apply pending migrations. The
  report path now threads the migration-free connection `run_worktree_doctor`
  opens (`open_database_without_migrations`) through every lookup. Pinned by
  `bare_doctor_applies_no_pending_migrations`: bare and `--json` doctor both
  succeed against a repository with a pending migration and leave it pending
  (`worktree list` re-applies it as the positive control). The duplicate-HEAD
  migration's recovery guidance builds on this: inspect with doctor first,
  then delete the duplicate rows by hand with the SQL statements the
  migration comment spells out (no repair spelling can be the route — every
  migration-applying path hits the same guard).
- **The `worktree repair --migrate-layout --dry-run` preview is now read-only
  in fact, not just in the docs.** The confirmation-free preview used to take
  the migration-applying CLI preflight and dispatch-time database opens, and
  resolved the global (migration-applying) connection before returning its
  plan — so a documented read-only check could irreversibly upgrade a
  repository's schema. The preview now skips every migration-applying open
  (CLI preflight plus both legacy and FUSE dispatch entries) and never
  resolves a database connection at all: `migrate_layout_run` defers the
  connection until after the dry-run return. Pinned by
  `migrate_layout_dry_run_applies_no_pending_migrations`: plain and `--json`
  previews both succeed against a repository with a pending migration and
  leave it pending (`worktree list` re-applies it as the positive control).
- **Read-only repair modes no longer create `.libra/maintenance.lock`.** The
  `--migrate-layout --dry-run` preview and every unconfirmed (would-be-refused)
  `worktree repair` were classified as repository writers, so the generic
  shared maintenance hold ran before dispatch and created the lock file — a
  filesystem write on paths documented as byte-for-byte side-effect free.
  Both modes are now classified read-only, so neither takes the hold, while
  every confirmed repair keeps its lock. Pinned by
  `read_only_repair_modes_create_no_maintenance_lock`: with the lock file
  deleted, plain and `--json` previews plus all four unconfirmed-repair
  refusals leave it absent (`worktree repair --confirm` re-creates it as the
  positive control).
- **The `--ignore-other-worktrees` refusal now names a recovery route
  (§C.13).** `checkout` and `switch` still never honor the flag (the same
  branch is never checked out in two worktrees). The `switch` CLI now
  actually parses the flag too (only `checkout` did before, so
  COMPATIBILITY's long-standing parity claim is finally true), but the
  refusal used to be a dead end. It now points at `libra worktree doctor` and
  `libra worktree
  repair --confirm` for a recorded owner that looks stale, pinned for both
  commands by `ignore_other_worktrees_flag_cannot_bypass_in_multi_worktree`.
- **Every user-visible `worktree repair` suggestion now names its
  confirmation.** CLI preflight hints, doctor findings, and recovery output
  across the crate pointed at bare `libra worktree repair …` commands that
  the W0 confirmation gate deterministically refuses. Each now carries
  `--confirm` (or `--yes` for `--resolve-identity`, `--dry-run` for the
  read-only preview), and the source-scan regression
  `repair_guidance_in_source_always_names_its_confirmation` keeps every
  backtick-quoted suggestion honest — including hints split across source
  lines, since the scan first joins Rust string-literal line continuations
  (`\`+newline) the way the compiler renders them — and the scan now covers
  the C.3.3 developer documentation sources, COMPATIBILITY.md, and every
  `sql/migrations/*.sql` comment as well (spans explicitly attributed to
  Git's own CLI are exempt; upstream has no `--confirm`).
- **The paginated `worktree.doctor` JSON examples now include the
  `worktrees[]` half.** The bare invocation has always serialized both the
  workspace page and the per-worktree findings in one envelope; the examples
  in `docs/commands/worktree.md` and its zh-CN twin showed only the
  workspace half. `worktree_doctor_json_schema_and_pagination_stable` now
  also pins the frozen top-level `data` key set and the per-worktree
  diagnostic key set.
- **Every mutating `worktree repair` action now requires `--confirm`
  (§C.11 W0, Codex R16/R17).** The no-arg registry repair, `repair <path>`,
  and a non-dry-run `--migrate-layout` previously mutated the registry,
  database and filesystem with no confirmation at all. Without the flag the
  command is now refused with `LBR-CONFLICT-002` before any side effect; with
  it the action runs inside one operation-log audit boundary, recording
  exactly one row per executed action (`libra op log`), success or failure.
  The read-only `--migrate-layout --dry-run` stays confirmation-free, and
  `repair <path> --resolve-identity` keeps its dedicated `--yes`. The FUSE
  surface (`--features worktree-fuse`) exposes and forwards the same
  `--confirm` flag. Pinned by
  the table-driven `worktree_doctor_mutations_require_confirmation_and_emit_audit`
  (no confirmation → zero side effects; confirmed → only the target scope
  changes; exactly one audit event).
- **`worktree repair <path> --resolve-identity` is now audited too.**
  `--yes` stays its dedicated confirmation (it is the one action that runs
  against an ambiguous registry), but once confirmed the detach runs inside
  the same one-row operation-log audit boundary as every other mutating
  repair — a success closes the row `succeeded`, a failure closes it
  `failed`. Previously the action detached the registry entry with no durable
  outcome record at all. Regression:
  `worktree_repair_resolve_identity_runs_inside_the_audit_boundary`.
- **A failed audit-boundary close now fails the repair command instead of
  being swallowed.** `finish_repair_operation` previously logged a warning and
  returned the repair outcome when the operation-log row could not be closed —
  silently violating the "exactly one row, closed with the outcome" contract.
  It now returns a fatal `LBR-IO-002` stating the repair completed but the
  record could not be closed, with a hint to inspect `libra op log`. Pinned by
  the fault-injection unit test
  `command::worktree::tests::finish_repair_operation_surfaces_close_failure`.
- **The no-arg FUSE-surface `Repair` now shares ONE audit boundary with the
  core registry repair** (`--features worktree-fuse`). Previously the legacy
  arm closed its boundary before `repair_fuse_worktrees()` ran: a FUSE
  failure followed a row already closed as successful, and a FUSE success
  went unrecorded. The FUSE surface now opens the boundary itself, runs both
  repairs inside it, and closes with the combined outcome. Regressions:
  `fuse_repair_shares_one_audit_boundary_with_core_repair` and
  `fuse_repair_failure_closes_the_shared_audit_row_failed`.
- **`fsck --heal` now fails closed on repositories that have (or had) linked
  worktrees.** Heal discovery walks only refs/reflogs/index roots — it does
  not consume the full `GcObjectSource` per-worktree inventory (private
  indexes, sequencer rows, sidecars, notes), so healing a multi-worktree
  repository could miss and mis-report another scope's objects. The command
  now refuses up front with `LBR-REPO-003` and points at
  `libra maintenance run` (the inventory-complete reachability walk), exactly
  like the W0 GC/repack/prune hard gate. Regression:
  `test_fsck_heal_refused_with_linked_worktrees`.
- **Worktree documentation now matches per-worktree HEAD semantics
  (§C.3.3/ADR-0714-09).** The `--ignore-other-worktrees` help text no longer
  claims worktrees share a single HEAD (it is accepted for Git parity but
  never bypasses the one-branch-one-live-worktree refusal);
  `worktree list --porcelain` integration assertions describe per-scope
  `HEAD`/`branch`/`detached`/`layout` lines; `for-each-ref`'s
  `%(worktreepath)` documentation now states the path comes from scoped HEAD
  rows across all worktrees; COMPATIBILITY's `switch` and `config` rows
  record the collision refusal (`LBR-CONFLICT-002`) and the probe-written,
  tighten-only per-worktree config overlay (public `--worktree` writes
  deferred as DEFER-09).
- **The repair confirmation/audit table test compares JSON semantically.**
  `worktree_doctor_mutations_require_confirmation_and_emit_audit` compared
  fixture bytes against on-disk JSON, but the migration writes struct-field
  order while the fixture round-trips through `serde_json` (alphabetical
  keys) — pure key-order noise. The assertion now parses both sides to
  `serde_json::Value`, so the W0 (§C.11, Codex R16/R17) confirmation+audit
  contract is pinned without false failures.

### Fixed (plan-20260714 R0-8 implementation review)

- **`status` now fails closed in text modes for EVERY blocked path.**
  An unreadable untracked directory's I/O-blocked event was marked
  "absorbed" and exempted from the fatal guard, so plain and `--quiet`
  status could return a normal verdict for a path it never inspected,
  violating the unconditional fail-closed contract (§B.6.0.1). The guard
  no longer exempts absorbed events; the `?? dir/` marker is still
  emitted (over-reporting is the safe direction), JSON keeps the partial
  contract (`io_blocked[]`, `base_scan_complete: false`), and the dirty
  cache is never rewritten from a guess. Regression:
  `quiet_unreadable_untracked_dir_fails_closed`.

### Fixed (plan-20260714 R0-6 implementation review)

- **Human `status` output now quotes paths like every other surface.**
  `build_human_entries` used `Path::display()`, so human output could emit
  literal TAB/LF/CR, backslashes, quotes and replacement characters for
  non-UTF-8 names, and `core.quotePath` had no effect. All human path
  rendering (including the unmerged section and aligned columns) now goes
  through the shared quoting helper.
- **`core.quotePath=false` no longer forces octal escapes for high bytes.**
  Invalid-UTF-8 path bytes above `0x7F` were always `\377`-escaped; they
  now pass through raw in the non-`-z` short/human/porcelain surfaces
  (Git parity), while `String`-typed message and JSON display strings keep
  the lossless escaped form. Control characters and `"`/`\` are escaped in
  all formats as before.

### Fixed (plan-20260714 R0-3 implementation review)

- **A metadata probe root hiding behind an escaping symlink is now reported,
  not silently skipped.** The probe checked the `.git`/`.libra`/gitlink
  exclusions before containment, so `status -- link/.git` (where `link`
  points outside the worktree and the target happens to hold a `.git`)
  vanished without a trace. Containment is now checked first and the escape
  lands in `io_blocked[]`.
- **Probe enumeration no longer gets a free entry per directory.** The
  readdir loop pulled the truncation-detecting entry past the budget check
  without charging it; every entry the OS yields is now charged exactly
  once against the enumeration budget.
- **The status I/O worker pool no longer detaches its threads.** Worker
  `JoinHandle`s were dropped at spawn time; they are now owned by the pool,
  matching the "reused pool, no detached threads" contract.
- **New regression coverage**: a `--check-dirty` MODIFIED cache row whose
  file stats fine but cannot be read is kept, reported in `io_blocked[]`,
  and never rewritten; the truncated-probe ordering test now asserts the
  qualified destinations through the rename records instead of the main
  scan's listing.

### Fixed (plan-20260714 R0-2 implementation review)

- **A regular→symlink swap during rename hashing can no longer forge an
  exact rename.** The worktree blob hash is produced through an open that
  follows symlinks, so a candidate replaced between the pre-read stat and
  the open could hand the exact gate a symlink referent's blob labelled
  `Regular`, pairing a rename that never happened and suppressing the
  truthful `D` + `??`. The file kind is now re-stated after the read and
  the candidate is dropped (with a degradation warning) when it no longer
  matches the snapshot stat. The residual path-level TOCTOU window is
  documented and stays with plan-20260715 WIO-02 for fd-bound resolution.

### Fixed (plan-20260714 R0-1 implementation review)

- **Exact rename selection is linear again for duplicate blobs.** Consumed
  destinations were removed from the OID bucket but not from the
  same-basename index, so N delete/add pairs sharing one blob id and one
  basename made every later source rescan them — Θ(N²) work before
  `renameLimit` could gate anything. Both levels now consume on pick and
  split destinations by evidence kind, keeping the stage O(N log N).
- **The `rename_limit_product_skipped` warning now matches the documented
  degrade contract.** It previously claimed "inexact matching" was skipped;
  in reality exact and scored unique-basename pairs survive and only the
  exhaustive stage is skipped, which is what the docs and the `diff` side
  already said. The status text now states the survivor semantics.
- **The untracked-rename pass accounts its budgets like every other pass.**
  `detect_renames_with_destinations` drew down the call-level worktree and
  object budgets but never restored them or recorded its comparisons, so a
  detection pass added after it would have restarted with fresh budgets.

### Fixed (`rebase`: replayed commits recorded a hardcoded identity)

- **Replayed commits now keep the original author and record the running user
  as committer.** All four branches of `create_replayed_commit` (`pick`,
  `fixup`, `squash`, `amend`) built their commit with `Commit::from_tree_id`,
  which hardcodes `mega <admin@mega.org>` as *both* signatures — so a rebase
  destroyed the original authorship and did not record who performed it. Per
  Git's semantics the author is preserved and never re-stamped (a fold keeps
  the target commit's author, i.e. the first commit of the group), while the
  committer is resolved through the same path as `libra commit`.
- **A missing identity is no longer reported as a corrupt object.** It was
  folded into the `CommitLoad` / `RepoCorrupt` classification, which pointed at
  the wrong fix; it now surfaces as `LBR-AUTH-001` with the
  `libra config user.name/user.email` hints, via the new
  `ReplayErrorKind::IdentityMissing` and `RebaseError::IdentityMissing`.

### Added (plan-20260714 W1: sequencer, rebase and bisect are worktree-scoped)

- **Sequencer control actions are recorded in the operation log.**
  `cherry-pick`/`revert`/`rebase`/`am`/`bisect` start, continue, skip, abort,
  quit, mark, reset and run now write an `operation` row through *boundary
  recording*: a short transaction takes a cross-process atomic claim, the
  control action runs outside any transaction, and a second short transaction
  records the outcome. The closure form of the wrapper could not be used —
  it holds a write transaction for the whole body, while every control action
  writes HEAD and refs through the pooled entry points, which deadlocks
  (`internal/head.rs`, `internal/branch.rs` document the rule).
- **One control action per worktree, enforced across processes.** The claim is
  a worktree-wide *slot* backed by a partial unique index, not a per-command
  key: two `am` starts with different patches, or an `am --continue` racing a
  `rebase --skip`, are different identities that would both have passed a
  per-identity check and then both replaced that worktree's single sequencer
  row, losing one sequence while its checkout stayed on disk. A claim left by
  a killed process is released only when its owner is *proven* gone (recorded
  `<host>/<pid>`), and the abandoned row is kept as failed rather than
  deleted — age alone is not proof of death, and a control action may sit for
  a long time in an editor or a hook.
- **`libra op restore` refuses operations it cannot actually restore.** The
  snapshot covers HEAD and refs; a sequencer control also moved an index, a
  working tree and sequencer state, so restoring one would move HEAD while
  leaving an in-progress sequence pointing at a todo that no longer matches.
  Such operations are recorded as non-restorable and refused with
  `LBR-CONFLICT-002` before the dry-run report. Operations that are still
  `running` — including a claim left behind by a crash — are refused too.

### Fixed (plan-20260714 W1)

- **A `cwd` change mid-command could write another worktree's sequencer row.**
  The scope was re-read from the process working directory at each layer, and
  `ChangeDirGuard` (or any library calling `set_current_dir`) moves it under a
  running command: a control action that read its state in worktree A could
  save it into whichever worktree the cwd had become, erasing A's sequence.
  Each invocation now pins its scope once at dispatch.
- **Two concurrent starts in one worktree could both proceed.** The start-time
  mutex was a check followed by an upsert, so two `bisect start` runs both saw
  no session, both checked out a candidate, and the loser's write replaced the
  winner's `orig_head` — leaving `bisect reset` to return HEAD to the wrong
  commit. The first write of a starting session is now an atomic claim against
  the scoped primary key; the loser gets the ordinary "already in progress"
  refusal, and an established owner still advances its own row.
- **Duplicate suppression no longer misreads a normal sequence.** `libra bisect
  good` twice in a row is how a bisect is driven, and the two invocations have
  byte-identical arguments. The dedup identity now covers the sequence position
  as well as the arguments, and control actions are exempt from the
  five-second succeeded-window heuristic altogether: re-running `rebase
  --continue` at an unchanged position is ordinary (the last one dropped an
  empty commit, a hook was fixed, an editor was aborted), and the worktree-wide
  control slot is a real mutex rather than a heuristic.
- **A second local HEAD row for a scope is refused rather than stored.** The
  W0 partial unique index made this an invariant; one internal test still
  asserted the old contract, where a detached HEAD could land beside an
  attached one.

### Fixed (HTTPS auth: a stored token was never attached to a request)

- **`libra auth login` stored a token that no request could use.**
  `HostScope::from_request_url` shared its parser with `HostScope::parse`,
  which refuses a path, query or fragment because a user-supplied *host*
  argument must not carry one. Every real request URL does: smart-HTTP
  discovery is `https://host/owner/repo.git/info/refs?service=…`. So the
  scope of a request was always `None`, the stored token was never attached,
  and `libra push`/`fetch` over HTTPS failed with `LBR-AUTH-001` however
  valid the stored credential was — while `libra auth status` reported it
  `valid`. The 401 guidance that should name the host printed the literal
  `<host>`, so the message could not even point at the right `auth login`.
  Request scope is now computed from host and port alone; the checks that do
  apply to a request — https (or http to a loopback host) and no credentials
  embedded in the URL — still apply, and the host-argument parser keeps its
  stricter rules. Regression:
  `internal::auth::tests::request_url_scope_survives_paths_and_queries`.

### Fixed (plan-20260714 R0-4 review: argv is `OsString`, warnings are per-invocation)

- **A non-UTF-8 argument killed the process.** `env::args()` panics on
  anything that is not valid UTF-8, so an ordinary pathspec naming a
  non-UTF-8 file aborted `libra status` with a Rust panic before clap ever
  saw it — and the error path in `main` had the same read, so it could panic
  while trying to report an error. The argv pipeline now carries `OsString`
  end to end (§B.4.3), and only the places that must interpret a value as
  text ask for UTF-8, failing there with a usage error about that one
  argument. `--find-renames=<non-UTF-8>` that a later occurrence overrides is
  therefore never interpreted at all; the same value as the winner is
  `LBR-CLI-002`; and a non-UTF-8 pathspec is refused by the parser with that
  code rather than panicking (`StatusArgs.pathspec` stays `String` for source
  compatibility — a documented narrowing).
- **Status warnings came from a process-wide buffer.** A long-running `libra
  code` server accumulates preflight advisories for its lifetime, so an API
  status collection reported warnings that had nothing to do with the
  request, and kept reporting them. The invocation's warning context is now
  passed explicitly: the CLI adopts the process buffer, the API starts empty.
- **A cache-mode status could still resolve a rename threshold.** clap
  refuses the flag combination, but `StatusArgs` is public: a struct-literal
  caller with `cached: true` and a threshold got a live threshold, and rename
  detection ran against a cache that cannot support it. Cache modes force it
  to `None` before anything else is considered.
- The argv scan records which format flags it SAW, interpreting short
  clusters through a merged root-plus-subcommand arity table, and refuses to
  run if it and the parser ever disagree — so a cluster-scan bug surfaces as
  a refusal instead of NUL separators nobody asked for.

### Fixed (plan-20260714 W0 review: deletion safety, scope guards, GC boundaries)

- **`libra file obliterate --recover` deleted object payloads without ever
  asking whether another repository was borrowing them**, and plain
  `obliterate` ran that same recovery pass *before* its own borrower check —
  so an interrupted obliteration was completed, objects unlinked, before
  anyone asked. The alternates-borrower gate is now one function called from
  the payload unlink itself, so no entry point can route around it, and every
  deletion surface (gc, repack `-d`, `cache evict` direct and scheduled,
  obliterate, obliterate recovery) goes through it.
- **`gc`, `repack` and `prune` failed on every shallow clone.** The
  reachability walk followed `parent_commit_ids` unconditionally, so the first
  absent parent past a `.libra/shallow` graft was reported as repository
  corruption. Shallow entries are now traversal boundaries: the boundary
  commit is kept, its absent parents are not demanded, and unparseable shallow
  metadata fails closed rather than silently resuming the old behaviour.
- **A pruned object's `object_index` row could outlive the object.** Loose
  files were unlinked first and the catalogue rows dropped afterwards, so a
  SQLite failure in between left rows advertising bytes that were gone. The
  order is inverted: the catalogue can now only under-advertise, which `agent
  doctor` rebuilds.
- **Nine ref writers consulted a fail-OPEN cross-worktree checkout probe** that
  folded a database error into "no other worktree has it", turning a transient
  query failure into permission to move or delete a branch another worktree was
  sitting on. That wrapper is deleted, so the fallible form is the only one
  available. `commit`/`amend` gained the guard they never had at all, running
  on the caller's own connection so the check and the ref write are atomic.
- **Nothing enforced one HEAD row per worktree scope.** Two concurrent
  detached-HEAD updates in one scope could both insert, after which the reader
  resolved the duplicate by returning an arbitrary row. Migration
  `2026072901` adds partial unique indexes and fails closed on pre-existing
  duplicates; the scoped read now reports the ambiguity instead of guessing.
- **`op restore` accepted an operation that ran in a different worktree**, and
  operations predating the scope column all claimed main scope. Migration
  `2026072902` records scope provenance and marks the genuinely
  unattributable rows `unknown`; restore refuses those and any cross-scope
  target before the dry-run report.
- **Three code paths minted a phantom `<working_dir>/.libra`** when storage
  resolution failed — creating a second repository, with its own `libra.db`
  and `objects/`, beside the real one. They now degrade to the read-only
  session they already fall back to, or fail closed with the repair route.
- **`hydrate` could resurrect an obliterated object**, because its fetch
  resolves through alternates and the durable tier, which may still hold bytes
  this repository erased. It now consults the tombstone snapshot first, and an
  unreadable tombstone table refuses the command instead of defaulting to
  "nothing was obliterated".
- A linked worktree whose identity the registry does not know now reports that
  fault and names `libra worktree repair`, instead of "HEAD reference is
  missing from storage" — which describes a corrupt repository the user does
  not have.
- Hints in `src/internal/workspace.rs` no longer direct users at
  `libra worktree doctor` *mutations* (`reclaim` / `adopt` / `release`) that do
  not exist until W4.
- **GC could delete an object that became reachable mid-run.** The one-hour
  mtime grace protects an object written moments ago, but not an OLD orphan
  that a concurrent `update-ref`, `reset`, `stash apply` or `op restore`
  republishes after the root scan has already decided it is unreachable.
  Nothing is deleted the first time it is seen unreachable now: a candidate is
  recorded, and only a later run that still finds it unreachable after the
  grace window deletes it — so it must survive two independent root scans,
  separated in time, with no reference appearing in between.
- **`FETCH_HEAD` was treated as a reachability root**, contrary to §C.4.3
  item 13 and to Git. `fetch` records the advertised tip of every ref it
  negotiated, including refs already up to date with no local destination, so
  rooting it pinned objects nothing in the repository referenced — permanently,
  and a little more on every fetch.
- **Agent-run findings manifests could be skipped silently.** A run
  interrupted before writing `manifest.json` — the exact state that leaves
  blobs with no other anchor — was read as "no roots", and the generic JSON
  walker skipped any OID whose object it could not find. The manifest is now
  parsed structurally (`findings_oid`, `manual_attach[].oid`), an absent
  object fails the run closed, a run directory with no manifest fails the
  walk closed at any age, and the directory scan, manifest size and
  attachment count are all bounded.
- **An invalid `scope_provenance` value failed open**, because `op restore`
  tested for the literal `"unknown"`. It now accepts only `"declared"`, and
  the database enforces the domain with triggers.
- Six user-facing error messages contained runs of fourteen spaces from
  collapsed line continuations.
- **Nothing stopped a worktree from publishing a reference to an object a
  concurrent deletion phase was about to unlink.** The two-scan quarantine
  proves an object was unreachable at two separated moments; it cannot prove
  nothing referenced it in between, and neither can a database transaction —
  the publications that matter here are FILES (a worktree's private index, a
  merge or rebase sidecar, an agent-run manifest), so staging content that
  happens to hash to a quarantined object commits without touching SQLite at
  all. A repository maintenance lock now supplies that exclusion: every
  command that can publish an object reference holds it shared for its whole
  run (derived from the scope inventory, so a new command cannot forget), and
  `gc`, `repack -d`, `cache evict`, `agent clean` and `file obliterate` hold
  it exclusively across "decide what is unreachable → invalidate the
  catalogue → unlink". Publishers never block each other; a deletion phase
  that cannot get exclusive access **defers** (the objects stay, the next run
  takes them) rather than deleting without the exclusion — except
  obliteration, which refuses outright, because reporting an erasure as done
  without performing it is worse than failing. A deferral leaves the
  quarantine clock untouched, so it never restarts the two-scan window.
- **The GC deletion phase held a SQLite read transaction across the whole
  reachability walk** — every commit and tree in the repository — blocking ref
  writers for the duration, and it unlinked files *inside* that transaction,
  so a failure part-way through rolled the catalogue back to advertise bytes
  that were already gone. The walk now runs under the maintenance lock with
  no transaction, and the catalogue invalidation is committed *before* the
  first unlink, in that order rather than wrapped around it.
- **The 4 MiB prune-ledger cap was checked only on read.** A ledger that was
  legal when loaded could grow past the cap during the run and be written
  anyway, after which every later run refused to read it — the quarantine
  clock stopped for a file this code had created. The size is now checked
  before the atomic replacement, and refusing leaves the previous, readable
  ledger in place.
- **A manifest that was valid JSON but not a JSON *object* was read as "this
  run declares no roots".** `[]`, `null` and a bare string all parse, every
  field lookup then returns nothing, and the findings blob the run owned
  became a prune candidate. The shape is now part of the contract.
- **A checkout collision detected at the storage seam was reported as
  repository corruption.** The seam guard added in this card raised
  `BranchStoreError::Corrupt`, which `symbolic-ref` maps to `LBR-REPO-002` —
  so a user racing two worktrees was told their repository was damaged, and
  any tooling keyed on the code would have escalated it. There is now a typed
  `CheckedOutElsewhere` variant carrying the occupying worktree, mapped to
  `LBR-CONFLICT-002` at every writer boundary, identical to the code the
  command preflights already returned.
- **The pooled HEAD-attach and branch-delete entry points ran their guard and
  their write as two separate implicit transactions**, so two worktrees could
  both pass the probe and both write. Both now open one transaction, matching
  `update_branch` — which also makes the branch-metadata cascade atomic with
  the ref delete instead of documenting the gap.
- **`Head::update_with_conn` logged HEAD-write failures and returned
  nothing.** `commit`, `reset`, `merge`, `rebase` and `bisect` all called it,
  so a failed HEAD update let the surrounding work commit and the command
  report success with HEAD pointing at the wrong commit — silently, and with
  the only evidence in a log nobody reads. The production form now returns
  `Result` and every caller propagates it; the swallowing variant survives
  only under `#[cfg(test)]`.
- **A checkout collision lost its classification on the way out of the
  storage layer.** `Branch::update_branch_with_conn` is called from inside
  reflog closures whose error type sea_orm fixes at `DbErr`, so the typed
  refusal cannot pass; by the time a command saw it, it had been wrapped
  twice more and each boundary re-classified it by hand. `switch -C` on a
  branch another worktree held reported `LBR-IO-002` — "failed to delete" —
  for a branch that was merely in use. The wording is now produced by one
  constructor and recognised by one predicate, both in `internal::branch`, and
  the boundaries ask that predicate instead of guessing.
- **Long-running sessions no longer hold the maintenance lock.** `libra code`,
  `automation`, `sandbox`, `service` and the agent surface are excluded: a
  session that runs for hours would starve every deletion phase, and a shell
  command the user approves *inside* that session could never satisfy "wait
  for the other command to finish" — the other command is its own parent.
  Excluding them opens no hole: an agent's VCS mutations go through
  `run_libra_vcs`, which spawns `libra` as a subprocess, and their in-process
  publications are already covered by the agent-run manifest fail-closed rule
  and the traces-inflight marker.
- **`maintenance` takes the lock per task, not per command.** `prefetch` runs
  the ordinary fetch writer in-process and publishes remote-tracking refs;
  `pack-refs` and `loose-objects` publish too. Carving out the whole command
  left them unprotected while a concurrent `repack` deleted an old pack.
  `repack` itself now publishes its consolidated pack under the shared hold
  and takes the exclusive one only for the deletion — so an obliteration can
  no longer classify an object as loose-only and then find it re-published
  inside a pack it never inspected.
- **`file obliterate` wrote its tombstone and audit record before taking the
  lock.** A contended lock was therefore discovered only after durable state
  existed, and the next unrelated obliteration would run recovery and complete
  an erasure this invocation had reported as refused. The lock is taken first,
  and it is re-entrant within a process so the re-check at the unlink sees the
  same hold.
- **A candidate that became reachable again kept its old quarantine
  timestamp.** If the new reference later went away, the very next `gc`
  deleted it on the strength of a window a reference had appeared inside.
  Resurrected candidates now leave the ledger and start over.
- **A deferred `incremental-repack` reported `objects_packed: 0`** although it
  had written the consolidated pack. Only the old-pack deletion is deferred,
  and the counts now say so.
- **The maintenance lock was tracked per process, not per repository.** A
  process holding repository A's lock could then publish into repository B
  re-entrantly, without ever opening B's lock file, while another process
  deleted B's objects. State is keyed by canonical lock path now — one
  process legitimately touches several repositories (alternates, a task
  worktree, an agent working on a clone).
- **`loose-objects` unlinks payloads**, so classifying it as a shared-only
  publisher was wrong. It follows `repack -d` now: shared through the pack
  write, then exclusive for the removals, deferring them if a publisher is
  running (the objects are already safe in the new pack).
- **`ReflogError`'s display dropped its cause**, so every failure that
  travelled through a reflog closure reached the command as a bare "failed to
  update reflog". The classification that lives in the underlying error's
  text went with it — a branch refused because another worktree had it
  checked out was reported as an I/O fault.
- **Two concurrent `repack -d` runs could fail the second one.** Both snapshot
  the same loose set; the first removes them, and the second treated the
  resulting `NotFound` as fatal despite having written a pack that contains
  every object. Already-gone is now the goal state, not an error.
- **`review`/`investigate` publish out of process-lifetime scope.** Their
  runs last minutes, so they are excluded from the command-level hold — but
  that left the objectize → manifest window unprotected, and a
  content-addressed attachment can resolve to an oid `gc` has already
  quarantined. The hold is taken at the publication seam itself, in both
  stores and both `attach` paths.
- **A pack was published without `fsync`, and its only other copy deleted.**
  `repack -d` and the `loose-objects` task unlink the loose objects the pack
  now holds; a pack that exists only in the page cache is not a copy, so a
  power loss between the write and the unlink took reachable objects with it.
  The writer now syncs the pack, its index and the directory entries naming
  them, and reports whether the NAME is durable — deleting callers keep the
  other copy when it is not.
- **`agent clean`, `worktree doctor` and `agent doctor --repair` each broke a
  rule this card had just written.** `agent clean` deletes object payloads
  and never passed the alternates-borrower gate; `worktree doctor` — whose
  contract is that a default invocation changes nothing — was classified as a
  writer and so created the maintenance lock file; `agent doctor --repair`
  republishes a findings blob outside any hold. All three are fixed, and the
  doctor read-only regression now compares a full recursive listing of
  `.libra` rather than three files that already existed.
- **An unreadable `objects/info/borrowers` read as "nobody borrows".** That is
  permission to delete objects another repository still needs. Absent is now
  distinguished from unreadable at the one gate every deletion surface calls,
  and an unreadable registration fails closed with `LBR-IO-002` — which
  scheduled maintenance propagates instead of folding into a successful run.
  Absence is likewise no longer proof that a borrower is gone (an unmounted
  path answers the same), so retiring one is an explicit act: the new
  `libra alternates prune [<path>] [--dry-run]`, documented EN/zh.
- A borrower refusal from `file obliterate` carried `LBR-OBLITERATE-003`,
  whose documented meaning is "re-run with `--yes`" — advice the user may
  already have taken, about a condition that has nothing to do with
  confirmation. It is `LBR-CONFLICT-002` now, at all three sites.
- `libra agent clean` now retires agent-run directories whose manifest is
  missing, unreadable, or not a JSON object, once they are past the retention
  cutoff. This is the explicit route
  the walk's fail-closed posture requires: without it, one interrupted run
  would make the repository permanently unprunable. A run whose manifest
  exists but is out of scope for this GC (a foreign `kind`, a non-terminal
  state) is untouched, as before.

### Added (plan-20260714 W0: read-only `worktree doctor`)

- `libra worktree doctor` reports per-worktree scope diagnostics — layout,
  identity, lifecycle state and what to do about each finding — and is
  **strictly read-only**: registry, database, lease state and filesystem are
  byte-identical before and after, pinned by a regression. Repair actions
  arrive as explicit subcommands in later waves, which is why no error hint
  promises that a bare `doctor` will fix anything.

### Changed (plan-20260714 W0: mutating-command scope inventory)

- Every command now declares a `CommandScope` (`Repository` / `Worktree` /
  `Composite` / `ReadOnly`) through an exhaustive match over all 108 `Commands`
  variants. A new command that does not declare its scope fails to **compile**,
  rather than failing a test that can be filtered out. The legacy-layout and
  corrupt-identity guards consult that inventory, which closes the holes the
  previous hand-maintained list had for `fetch`, `apply`, `rerere`, `mv`,
  `clean`, `restore`, `reset`, `rm`, `stash` and `pull`.

### Fixed (`merge`: merge commits recorded a hardcoded identity)

- **Merge commits now carry the configured `user.name` / `user.email`.** Every
  merge-commit path — the plain three-way merge, `--no-ff` / `-s ours`, and
  `merge --continue` — built its commit through `Commit::from_tree_id`, which
  hardcodes `mega <admin@mega.org>` as both author and committer. The
  repository identity (and the `GIT_AUTHOR_*` / `GIT_COMMITTER_*` date
  overrides) was silently discarded, so merge commits were misattributed while
  every other commit was correct. Merge now resolves its identity through the
  same path as `libra commit`, and reports the same actionable error
  (`LBR-AUTH-…`, with the `libra config user.name/user.email` hint) when no
  identity is configured.

### Added (`merge --continue -m <msg>`)

- **`libra merge --continue` accepts `-m`/`--message`** (a Libra extension —
  `git merge --continue` takes no arguments). Libra finalizes a continued merge
  without opening an editor, so the message recorded when the conflicted merge
  started was previously unreachable; `-m` now overrides it. Without `-m` the
  stored message is replayed exactly as before.

### Added (plan-20260714 W4: `worktree doctor` — read-only scope diagnosis)

- **`libra worktree doctor [<workspace-id>] [--limit N] [--cursor C]`** reports
  what is wrong with each Agent workspace scope and repairs nothing. Every
  invocation is strictly read-only: no row, registry entry, lease, or file is
  written (pinned by a before/after comparison of the row dump, the registry
  bytes, and the repository's file tree). Without an id it pages
  `data.diagnostics[]` plus an opaque `data.next_cursor` (`workspace_id`
  ascending, default limit 50, capped at 500); with an id it returns the
  singular `data.diagnostic` and no pagination keys — combining the two is a
  usage error.
- **Records written under a previous repository identity are visible here and
  nowhere else.** They are exactly the rows the identity-scoped listings hide
  while they block new workspace registrations, so the doctor query is
  deliberately not identity-filtered; each diagnostic carries its own
  `repo_id`.
- **Two new stable codes, `LBR-WORKTREE-001` and `LBR-WORKTREE-002`** (both in
  the existing `repo` category, exit 128). A pagination cursor this command
  did not issue is refused rather than silently restarting at page one — a
  caller walking the registry page by page would otherwise re-read rows and
  believe it had seen everything. A scope that cannot be read (unparseable
  registry, unreadable record, missing repository identity) fails closed
  instead of answering with a partial diagnosis.
- **Workspace hints that mention the doctor no longer promise recovery actions
  the CLI cannot perform.** The mutating grammar (reclaim/adopt/clear) is not
  shipped, so every such hint is now inspect-only wording, pinned by a
  regression that scans the sources for the forbidden verbs.

### Changed (plan-20260714 R0-1: `diff` now uses the shared rename engine)

- **`diff` delegates rename pairing to `rename_detect::match_pairs`.** It had
  carried its own exact / unique-basename / renameLimit / top-K / greedy
  implementation, which meant two sets of tie-breaks, two budget accountings
  and two eligibility rules — and they had drifted, so the same repository
  could report different renames depending on which command you asked. The
  remaining diff-side code builds a `RenameSnapshot`, supplies a
  `RenameContentSource` over its existing loaders, and translates the result
  back into diff entries. Behavior is unchanged for every case the suites
  cover; what goes away is the second implementation that could drift again.

### Fixed (plan-20260714 R0-1 / R0-2 review round 11)

- **A corrupt HEAD no longer panics `status`.** The cache-mode path used the
  lossy `Head::current()` wrapper, so a reference row that failed validation
  — including one whose OID belongs to a different hash algorithm — aborted
  the process instead of reporting an actionable error. `worktree list
  --porcelain` likewise swallowed the error with `.ok().flatten()` and
  printed a successful listing with the HEAD lines silently missing.
- **The whole scoring batch shares one 5 s deadline and one OID cache.**
  Resuming a budget rebuilt both, so the second detection side received a
  fresh 5 s and re-read objects the first side had already fetched.
- **`diff`'s exact buckets are ordered by new-path bytes**, so a
  non-lexicographic input cannot pair differently in `diff` than in `status`.

### Fixed (plan-20260714 R0-5 review PASS)

R0-5 (porcelain v2 rename records) passes Codex review. Everything the card
claims is now pinned by a regression: staged `R.` with a real mode flip,
unstaged `.R` with real `100755` columns from an executable fixture, chained
renames asserted by TOTAL change-row count in both configurations (so a
leaked endpoint row or spurious `.R` cannot hide behind a correct `2 ` count),
`-z` raw bytes, records from a subdirectory carrying real metadata, `MR` when
the rename source also has a staged change, and fail-closed handling of an
unreadable — as opposed to absent — worktree mode.

### Fixed (plan-20260714 R0-1 / R0-2 / R0-5 review round 9)

- **A single `status` no longer spends its read budgets twice.** The staged
  and unstaged passes each built fresh `ObjectReadBudget`/`WorktreeReadBudget`
  instances, so one invocation could use 2 × 500k comparisons and 2 × 64 MiB
  of reads while each side individually looked compliant. The remaining
  amounts now travel between the two passes.
- `Cargo.lock` is kept in step with the version bump, so the release
  workflow's `cargo build --locked` cannot fail on a stale lockfile.

### Fixed (plan-20260714 R0-1 / R0-2 / R0-5 review round 8)

- **A porcelain v2 rename record with missing SCORE metadata fails closed**
  instead of defaulting to `R100`. `R100` is the documented spelling of an
  exact rename, so the default published inexact pairs as byte-identical —
  the mode and hash columns already refused to guess.
- **Every HEAD deserialization boundary validates the OID's hash algorithm**,
  not just the main one: linked-worktree and remote detached HEADs went
  through an unchecked parse. All three now share one
  parse-plus-kind-validate helper, so a new boundary cannot silently skip it.
- **`diff.renameComparisonBudget` bounds the unique-basename stage too.**
  Enforcement began only in the exhaustive loop, so `=1` could still score
  arbitrarily many basename pairs and, if they consumed every candidate,
  report no degradation at all.

### Fixed (plan-20260714 R0-1 / R0-2 / R0-5 review rounds 5-7)

- **An unstaged rename whose SOURCE also has a staged change renders `MR`,
  not `.R`.** Edit `a`, `add a`, then move it to `b`: the record used to
  claim `.R` — losing the staged modification entirely (the endpoint row is
  suppressed) and copying the index hash into `hH`, asserting HEAD and index
  agree when the user had just changed the index.
- **Rename pairing is decided GLOBALLY, same-basename first**, in both
  `status` and `diff`. Walking sources in path order let a source with no
  name match claim the destination another source shared a name with.
  Implemented as two linear passes with consumed destinations removed from
  their bucket, so a tree full of duplicate blobs is not quadratic.
- **The worktree read budget cannot be bypassed by a growth race.** Bytes
  that were read are charged even when the result is refused as over-cap;
  previously 4096 candidates could each read ~2 MiB while the 64 MiB total
  budget stayed untouched. The `status` comparison budget now also bounds the
  unique-basename stage rather than only the exhaustive one.
- **A ref carrying an OID of the wrong hash algorithm fails closed at the
  read.** A well-formed SHA-256 id in a SHA-1 repository parsed cleanly and
  only failed much later, as a panic inside object loading.
- **A type change between the snapshot stat and the OID read is detected.**
  A regular file replaced by a symlink would otherwise hand the exact gate a
  symlink-target OID labelled `Regular`.
- Optional worktree stats and symlink reads now run under the same I/O
  deadline as every other worktree read.

### Fixed (plan-20260714 R0-1 / R0-2 review round 4)

- **SHA-256 repositories detect unstaged exact renames again.** The worktree
  OID is hashed on a pooled I/O worker, and the repository hash kind is
  thread-local — a worker started at the SHA-1 default, so its id could never
  equal the SHA-256 index entry. The pair silently degraded from exact to
  inexact and read objects it did not need. The hash kind now crosses into
  the worker with the job.
- **`diff` applies the shared engine's inexact ELIGIBILITY and budget
  accounting.** Symlinks no longer enter basename or exhaustive similarity
  scoring (their "content" is a target string, which `status` never scores),
  and unique-basename comparisons are charged against the same
  `diff.renameComparisonBudget` the exhaustive stage spends instead of
  restarting the count at zero.

### Fixed (plan-20260714 R0-1 shared-scorer parity)

- **`diff` and `status` now report the SAME rename pairings.** Both apply the
  shared engine's exact-bucket rule — sources consumed in path-byte order,
  each preferring a same-basename destination and otherwise the byte-smallest
  candidate — and both break exhaustive-stage ties on path bytes rather than
  on transient vector indexes. Previously `diff` took whichever destination
  happened to be enumerated first, so a repository with duplicate content
  could report one set of renames from `status` and a different set from
  `diff`.
- **Porcelain v2's rename record syntax is documented correctly** in the EN
  and zh status docs and the CHANGELOG: `2 <XY> <sub> <mH> <mI> <mW> <hH>
  <hI> R<score> <new>\t<old>`. The old text wrote `2 R<score> …`, conflating
  the second field (`R.` staged-only, `.R` unstaged-only) with the ninth.

### Fixed (plan-20260714 R0-2 / R0-5 / R0-6 review closeout)

- **`status --porcelain=v2` rename records from a SUBDIRECTORY report real
  metadata again.** The payload is projected to repository-root paths before
  rendering; the v2 writer projected them a second time, so `sub/a.txt`
  became `sub/sub/a.txt`, the HEAD/index lookups missed, and the record
  failed closed instead of carrying its mode and hash.
- **A rename record fails closed on an UNREADABLE worktree mode** instead of
  fabricating `100644`. A genuinely absent destination (a chained rename
  already moved it) still renders `000000` — "gone" and "cannot read" are
  different answers.
- **`--porcelain --ignored` / `--short --ignored` honor `core.quotePath`**,
  and porcelain v2 `u` records carry raw path bytes under `-z` and the
  escaped form otherwise; both previously used `Path::display()`, which
  neither escaped control characters nor preserved non-UTF-8 bytes.
- **A rename candidate that disappears between the scan and the hash is
  reported as a degradation.** It used to be dropped silently, leaving
  `rename_detection_complete: true` for a pairing that was never attempted.
- **LFS worktree reads are byte-capped and size-stable**, so a file that
  grows after its size check can no longer blow the read budget or produce a
  pointer describing content that never existed.

### Fixed (plan-20260714 R0-3 / R0-7 / R0-8 / R0-9 review closeout)

- **`status` never reports a repository it could not fully inspect as
  clean.** The fail-closed guard moved out of the renderer, so `--quiet`
  (which skips rendering) and both dirty-cache fallback paths now refuse
  with `LBR-IO-001` instead of exiting 0. `--check-dirty` re-verification
  runs one tri-state stat per row — a permission change mid-run can no
  longer be read as "the file is gone" and prune a still-valid cache row —
  and a blocked re-verification writes nothing at all.
- **`status --scan` no longer walks the worktree twice.** The cache
  snapshot is built from the same bounded, `io_blocked`-aware scan that
  produced the status, with rename detection disabled for the snapshot
  (the cache stores paths by kind and has no rename row: pairing them
  would have persisted an empty snapshot for a renamed file, and the next
  `--cached --exit-code` would have called the repository clean).
- **Every blocking worktree read is now time-bounded** — tracked stat and
  content hash, untracked enumeration, ignore lookups, and the probe's
  directory listing and per-entry metadata — by a pool of 8 REUSED worker
  threads. The deadline measures lack of progress, so a large directory
  that keeps yielding entries is never mistaken for a hung mount.
- **`io_blocked[]` and `warnings[]` are one-to-one**, including on the
  cache paths; repository-level preflight advisories are folded into the
  same list as `repository_preflight` / `config`, so `--json
  --exit-code-on-warning` can never exit 9 with an empty `warnings[]`,
  and `--json` keeps stderr clean. `base_scan_complete` and
  `rename_detection_complete` now report their own subsystems
  independently.
- **Non-UTF-8 names never fail `status`** in any mode, including `--scan`
  and the cache modes, and JSON paths render through the documented
  escaping instead of `Path::display()` — two different filenames can no
  longer collapse onto one JSON string.
- **The probe excludes repository metadata, gitlinks and nested
  repositories unconditionally**, including when a pathspec names one
  directly, and reports a resolved-outside-the-worktree path as blocked
  rather than skipping it silently.

### Added (Part C W4)

- **`libra agent workspace list|show` (plan-20260714 Part C W4 machine
  interface)**: the read-only surface over the W4 workspace registry —
  keyset-paginated `list` (`workspace_id` ASC, default limit 50 capped
  at 500, opaque `next_cursor`, repeatable `--state` filters) and a
  by-id `show`, both emitting frozen schema-v1 records (kind, state,
  owner, lease fence/expiry, canonical path, task/session
  associations). Lease mutation stays internal to the agent runtime
  services.

### Changed

- **Rename-degradation semantics are now spelled out (plan-20260714
  R0-9)**: when `diff.renameLimit` / `status.renameLimit` is exceeded on
  either side, exact renames AND unique-basename renames are still
  reported — only the exhaustive inexact stage is skipped, with a
  `rename_limit_product_skipped` warning.
  `diff.renameComparisonBudget` (or status's similarity budget) is
  charged per comparison across BOTH the unique-basename and exhaustive
  stages: when it is spent, the stage then running stops, no further
  comparisons happen, the exhaustive pass's results are discarded
  wholesale (a partial exhaustive result would be order-dependent), and
  only pairs already scored — exact plus the unique-basename pairs
  already paired by then — survive, with a `similarity_budget_exceeded`
  warning. `diff` now runs the same staged order as `status`
  (exact → unique-basename → bounded exhaustive), so a degraded run can
  no longer demote a basename-proven rename to a delete + add pair.

### Fixed

### Added (PD-03)

- **Session-erasure cloud tombstone propagation (plan-20260714
  PD-03)**: `libra cloud sync` now publishes the local
  `agent_import_tombstone` rows (written by `erase_session_local`) to a
  new D1 `agent_import_tombstone` table under the same generation fence
  as the capture catalog — an idempotent fenced UPSERT (newest
  `erased_at` wins, known fingerprints kept) followed by a cascade
  delete of the erased session's mirror rows (claims → revisions →
  links → checkpoints → session). `libra cloud restore` is
  tombstone-first: erased sessions and their companions never restore,
  and the tombstones are persisted locally on the restoring machine so
  a later import cannot resurrect the session there either; pre-PD-03
  remotes without the table restore unchanged (read-only tolerance).
  Repeated deletes and restores are idempotent, and the audit log stays
  outside regular GC as before. The only remaining deferral is R2
  physical payload deletion. Verified end-to-end against a real D1
  endpoint (`agent_cloud_tombstone_test`, now asserting propagation);
  the A0-10 doc-guard flipped to pin the new facts.


- **PD-09 adversarial-review batch (19 confirmed findings)**: the mbox
  envelope test is now ctime-shaped (prose `From …` lines never
  false-split; timezone/UUCP-suffixed envelopes split correctly) and
  body `>From ` quoting is preserved byte-for-byte (git-default mboxo
  reading). The apply seam gains git parity on: empty-file
  creation/deletion patches, umask-derived modes for new files (no more
  0600), real symlink materialization for `120000` sections (including
  symlink-content bases), delta preimage verification against the
  `index` header's old blob id (a same-size modified base now refuses
  instead of corrupting), chained same-path sections keeping earlier
  mode overrides, and `-p0` keeping `diff --git` header names verbatim.
  `am -3` pre-resolves base blobs through the full object store (packed
  objects and abbreviated ids now work in clones), a conflicted pause
  stages the mail's mode overrides and persists the applypatch-msg
  hook's message edit so `--continue` commits both, a pause flag makes
  an unstaged "resolution" error out instead of silently re-clobbering
  the file with markers, and `am --abort` refuses to hard-reset away
  commits made on top of the paused state. Multipart parsing accepts
  RFC 2046 transport padding on boundary lines, empty-header parts, and
  quoted `;` inside Content-Type parameters. (One accepted limitation
  documented in review: the hook message file shares the worktree
  `COMMIT_EDITMSG` slot.)

### Added

- **`am` applypatch hooks (plan-20260714 PD-09 ⑤ — PD-09 complete)**:
  `applypatch-msg` (may edit the proposed commit message via the
  worktree `COMMIT_EDITMSG`, non-zero refuses the mail before any
  worktree write), `pre-applypatch` (gates the commit after write +
  stage, including the resolved `--continue` path), and
  `post-applypatch` (advisory) now run from `.libra/hooks` through the
  sandboxed repository-hook runner. Every refusal leaves the saved
  series resumable (`--continue`/`--skip`/`--abort`), and
  `LIBRA_NO_HOOKS=1` bypasses. This closes the last PD-09 slice: am's
  deferred surface is down to Git's wider flag set.

- **`am`/`mailinfo` MIME multipart mails (plan-20260714 PD-09 ④)**: the
  shared mail parser now handles `multipart/mixed`/`alternative`
  containers (nested, bounded depth) — parts split on the declared
  boundary, every supported text part (`text/plain`, `text/x-patch`,
  `text/x-diff`) decodes with its own transfer encoding
  (7bit/8bit/base64/quoted-printable) and concatenates in order, HTML
  alternatives and binary attachments are skipped, and a multipart mail
  with no supported text part fails closed. `format-patch --attach`
  output — Libra's own and real Git's (git-gated round-trip) — now
  applies directly with `libra am`.

- **`am -3`/`--3way` three-way fallback (plan-20260714 PD-09 ③)**: a
  text patch that does not apply falls back to a three-way merge — the
  base is the `index` header's old blob resolved from the local
  loose-object store (abbreviated ids resolve only when unambiguous),
  theirs is the patch applied to that base, ours is the current
  content. A clean merge applies silently; a conflicting one writes
  `<<<<<<<` markers into the worktree and pauses under the existing
  sequencer semantics (resolve + `--continue`, or `--skip`/`--abort`).
  A base that is not locally present keeps the plain refusal — the
  fallback never fabricates content — and the `-3` choice persists in
  the saved series state across resumes.

- **`am`/`apply` binary, rename, copy, and mode-only patches
  (plan-20260714 PD-09 ②)**: the shared apply seam now understands git
  extended sections beyond plain text hunks — `GIT binary patch`
  payloads (both `literal` and `delta`, decoded from git base85 + zlib
  with bounded inflation and delta-op bounds checks), `rename
  from`/`rename to` (with or without content hunks; the source deletion
  and destination land in the same commit), `copy from`/`copy to`, and
  mode-only `old mode`/`new mode` changes (the executable bit is
  applied to the worktree AND staged directly, since a content-equal
  flip is invisible to the worktree-change scan). Every extended target
  passes the unchanged path-safety surface (absolute/`..`/`.libra`/
  symlink components refused — hostile rename destinations included).
  `apply --check` validates the same extended sections. Verified
  against real `git format-patch --binary` output (binary add, rename,
  chmod-only) via the git-gated round-trip suite.

- **`am` stdin and mbox input (plan-20260714 PD-09 ①)**: `libra am -`
  reads one mail or a whole mbox from standard input (allowed at most
  once, bounded by the shared 64 MiB series cap), and any input — file
  or stdin — whose first line is an mbox `From ` envelope is split into
  its messages and applied in order. The envelope test is conservative
  (`From ` + token + a date tail ending in a 4-digit year, the
  `git format-patch` shape), mboxrd `>From ` body quoting is undone,
  and multi-message sources are position-labelled `<source>#<n>` in
  output and sequencer state. The full mail content persists in the
  sequencer state, so a stdin-sourced series supports `--continue` /
  `--skip` / `--abort` exactly like file-sourced ones. Verified with a
  real `git format-patch --stdout` → `libra am -` round-trip gate
  (skips when git is absent).

- **Checkpoint-scoped review/investigate (plan-20260714 PD-02)**:
  `libra review --agent <slug> --checkpoint <id>` and
  `libra investigate start --topic <text> --agent <slug> --checkpoint
  <id>` now run against a captured agent checkpoint instead of failing
  closed. The reviewers'/investigators' whole workspace is the
  checkpoint's own content — `metadata.json`, an optional
  `manifest.json`, and the transcript files — materialized READ-ONLY
  under the run directory (`<run_dir>/checkpoint-input/`, per-file
  64 MiB / 256 MiB total caps, path-component sanitization). No
  repository snapshot is materialized at all, so a scoped run can only
  consume the named checkpoint, and the scoped prompt frames the
  content as a captured transcript whose text is untrusted data — never
  a worktree diff. A missing checkpoint, malformed checkpoint tree, or
  content absent from the local object store fails closed BEFORE any
  run is created (no run residue). The investigate scope persists in
  the run state, so `investigate continue` re-materializes the SAME
  checkpoint and can never fall back to the current worktree; the
  materialization shares the run's retention (`review/investigate
  clean`) and orphan-release surface, and `agent doctor` reports zero
  new findings for scoped runs.

- **`status` io_blocked partial contract, frozen warning schema, exit
  arbitration, and the non-UTF-8 posture (plan-20260714 R0-8/R0-9)**: a
  path `status` cannot inspect (EACCES/I-O failure) is no longer
  fabricated as a deletion or as clean — text formats fail closed with
  `LBR-IO-001` naming the first blocked path, while `--json` succeeds
  and reports the partial result through `data.io_blocked[]`
  (`{path:{display,raw_base64},staged,reason,rename}`, raw-byte-sorted
  and deduplicated) plus `base_scan_complete` /
  `rename_detection_complete` / `complete`. The unstaged rename-destination
  probe is bounded by two call-global budgets — 50,000 enumerated entries
  and 10,000 qualified destinations, aggregated across probe roots — and
  tripping either keeps the pairs found so far while emitting a
  `probe_truncated` warning (`source: probe`) that names which budget
  ran out. The budgets apply ONLY to the probe: the display scan still
  reports every untracked path; `is_clean` is `false` and
  `--exit-code` reports dirty while anything is blocked, a blocked
  `--scan` refuses to replace the dirty cache, and `--check-dirty`
  keeps rows it cannot re-verify. Structured warnings now use one
  frozen `{code, message, source}` schema (sources
  `config`/`probe`/`rename_detect`/`worktree`/`metadata`/`cache`, codes
  documented in `docs/commands/status.md`; `probe` is distinct from
  `rename_detect` so consumers can tell "candidates may never have been
  seen" from "seen but unscorable"), one warning per `io_blocked[]`
  entry, object-read faults during rename scoring
  (missing/corrupt/unavailable objects, budget caps) skip only the
  dependent inexact candidates with deduplicated
  `metadata_unavailable`/`metadata_budget_exceeded` warnings, and exit
  arbitration is fatal ≻ 9 (`--exit-code-on-warning`) ≻ 1 (dirty) ≻ 0
  on every output path — including the dirty-cache stale-fallback
  path. Non-UTF-8 path names never fail `status` anymore: the base
  `??` row is kept (RAW OS bytes on the `-z` wire on Unix — short and
  porcelain v1/v2 `-z` records now write exact path bytes — and
  octal-escaped readable display elsewhere, including
  `io_blocked[].path.display`), while rename candidacy alone is
  skipped with a `rename_path_encoding_unsupported` warning
  (non-UTF-8 rename scoring stays a deferred extension).

- **`status` short/porcelain path quoting + public entry API
  (plan-20260714 R0-6/R0-7)**: `core.quotePath` is honored through the
  strict local→global→system cascade (strict Git boolean, default
  `true`, invalid values fail closed) — human-short and non-`-z`
  porcelain v1/v2 paths now C-style-escape control characters, `"` and
  `\` unconditionally and non-ASCII bytes as `\ooo` octal under the
  default; `-z` records keep raw unquoted bytes. New public API
  `ShortStatusEntry` / `generate_short_status_entries` exposes the
  rename-aware short entry list (renames first-class, destination
  worktree state in the unstaged column) and now feeds both the short
  and porcelain-v1 renderers. The legacy `generate_short_format_status`
  tuple API is RESTORED to its pre-R0 shape: renames decompose into
  endpoint states (staged old=`D `/new=`A `, unstaged old merges an
  unstaged `D`, destination becomes `??`), guarded by the new
  `compat_legacy_short_api_pre_r0_equivalence` target. `status` also
  resolves `status.renameLimit` (falling back to `diff.renameLimit`)
  through the same cascade.

- **Task worktrees take a workspace lease (v0.19.62,
  plan-20260714 Part C §C.8, W4 slice 2)**: the scheduler's task
  worktrees are now first-class workspaces. Provisioning publishes an
  `active` `workspace_record` for the materialized directory — after
  materialization, never before, so a crashed provision leaves no fake
  active record — associated with the task id and owned by a lease
  scoped to that task and process. Sync-back RENEWS that lease first
  and fails closed if it was reclaimed: replaying a task worktree's
  changes into the main workspace while a doctor has handed the
  workspace to someone else is exactly the double-write the lease
  exists to prevent. Teardown moves the record to `releasing` before
  touching the filesystem (so nothing claims the directory
  mid-removal), settles it as `released` on success, and marks it
  `orphaned` when cleanup fails, leaving the scavenger a record instead
  of a silent leak. Two parallel task attempts get independent
  workspace ids, paths and lease owners. A working directory that is
  not a Libra repository still gets a task worktree — there is nothing
  to associate it with, so no record is published — while a repository
  whose database cannot be claimed fails provisioning rather than
  running unregistered.

- **Agent workspace association + lease store (v0.19.61,
  plan-20260714 Part C §C.8, W4 slice 1)**: a new `workspace_record`
  table (migration `2026072501`) and internal `WorkspaceStore` service
  give linked worktrees, task copy/FUSE worktrees, and future remote
  workspaces one queryable association layer — and one place that
  coordinates their *writers*. A linked workspace's lease identity is
  `(repo_id, worktree_id)` and the canonical path is unique across every
  live workspace, both enforced by partial unique indexes: `acquire`
  writes first and maps the resulting conflict onto `LBR-AGENT-022`
  ("lease held"), so two agents racing for one worktree — or one
  directory reached through a `.`/`..`, trailing-separator, or symlink
  alias — can never both publish a record. `repo_id` is never taken from
  the caller: it comes from the repository's own `libra.repoid` through a
  `RepoIdentity` token (a padded or empty value is refused as corrupt
  metadata rather than normalized, so the SQL guard and the Rust reader
  can never disagree), it is resolved before the transaction opens so
  every mutating transaction stays write-first (a read-then-write
  transaction hands the loser a transient "database is locked" instead
  of the stable refusal), and a rewritten identity fails closed while
  workspaces of the previous identity are still live or awaiting
  recovery instead of silently reopening the uniqueness namespace.
  Rows stranded by a rewrite stay reachable: a bounded, keyset-paginated
  recovery listing is the only read that can see them, and adopting one
  re-homes it onto the current identity with its state, owner and fence
  intact. Workspace paths must be absolute, are resolved entirely by the
  kernel (so `link/../ws` cannot alias its way past the index), and
  require a resolvable parent, so a directory that does not exist yet
  cannot be claimed twice through a dangling symlink; publishing a
  workspace as `active` additionally requires the directory to exist,
  since `provisioning` is what covers materialization. The worktree
  registry now shares that kernel-first path resolution, so
  `worktree add a/link/../wt` lands where the kernel says it does
  instead of at a lexically collapsed path. Renew/activate/release are
  owner + monotonic-fence conditional writes: after a doctor reclaim
  mints a higher fence, the previous owner's calls refuse with
  `LBR-AGENT-023` instead of releasing (or clobbering) the new owner's
  lease. An expired lease is never stolen implicitly — only the
  doctor/scavenger reclaim path takes it over, and only once the
  deadline has passed — while a human using a linked worktree takes no
  lease at all and is never locked out. Failed provisioning or cleanup
  marks the record `orphaned` (identity freed, row kept for diagnosis
  and bounded scavenging) and a released record frees its identity for a
  fresh acquire with a new workspace id. The table stores association
  IDs only — no prompts, transcripts, or tool payloads — and listings are
  keyset-paginated (default 50, cap 500). The rollback of `2026072501`
  refuses while any non-terminal workspace exists, which is also how live
  leases block the deeper `2026072402` rollback. No user-facing command
  surface yet: the runtime/CLI wiring lands in the following W4 slices.

### Changed

- **Internal: self-maintaining migration rollback expectations and
  cleanup-test scope isolation (v0.19.63)**:
  `tests/agent_capture_migration_test.rs` now derives its expected
  rollback/reapply version lists from `builtin_migrations()` via a
  `registered_versions_after` helper instead of repeating hardcoded
  version lists, so landing a new migration no longer requires editing
  this file, and two call sites use the canonical
  `run_builtin_migrations` helper rather than a hand-built
  `MigrationRunner`. The authoritative pinned full-list guard in
  `tests/db_migration_test.rs` (literal versions plus
  `max_registered_version`) is unchanged. The `history.rs` unit tests
  for rejected-checkpoint cleanup and inflight-marker repair now run
  through `ClientStorage::with_background_index_failure_scope`,
  matching the CLI invocation-local object-index failure scope so an
  unrelated test cannot consume another operation's finite drain
  budget. Test-only; no behavior change.

- **Legacy-layout detection and `repair --migrate-layout` (v0.19.60,
  plan-20260714 Part C §C.6, W3 slice 3)**: `worktree list` now reports a
  per-entry `layout` (`main`/`linked-v2`/`legacy-symlink`/`missing`/
  `corrupt`, JSON field + porcelain `layout` line). In a legacy
  shared-`.libra` symlink worktree, read-only commands keep working but
  the worktree-state mutation surface refuses with `LBR-REPO-003` and a
  migrate hint (mutating there would move MAIN's HEAD/index).
  `worktree repair --migrate-layout [--dry-run] [<path>]` migrates legacy
  worktrees from the main worktree via a journaled, identity-checked
  state machine (migration 2026072403): prepared journal-stamped gitdir,
  atomic renames with the legacy link kept as a backup until
  verification, detached HEAD at the shared snapshot, private index
  rebuilt from that commit — working files untouched, shared staged
  state never copied; unmerged shared index or an active main sequencer
  refuses before any rename, and `worktree repair` recovers every crash
  window by identity, keeping materials on any mismatch.

- **Register the Claude Code `PreToolUse` hook (v0.19.60,
  plan-20260713 DR-00)**: `libra agent enable --agent claude` now
  installs a forward for `PreToolUse` in addition to `PostToolUse`, both
  routed to `libra hooks claude tool-use`. This aligns the installer with
  the config already documented in `docs/commands/hooks.md`; the parser
  already mapped both events to `LifecycleEventKind::ToolUse`, so the
  change is installer-only. A `ToolUse` event refreshes `agent_session`
  liveness on the capture/traces path and writes **no** checkpoint
  (checkpoints materialize only at `Stop`/`SessionEnd`), and it does not
  touch the AiIntent `tool_use_count` stream (installed Claude hooks use
  the AgentTraces path). No `Subagent*` boundary event is registered, so
  Claude's on-disk sub-agent content stays `unresolved` (DR-06). Existing
  five-event installs gain the PreToolUse forward on the next
  `enable`/upsert; user-owned hooks are preserved.

- **Canonical `worktree add` targets: branch, commit, `--detach`, `-b`
  (v0.19.59, plan-20260714 Part C §C.7, W3 slice 2)**: `worktree add
  <path> [<branch-or-commit>]` checks an existing branch out ATTACHED
  (refused before any side effect when any worktree — including the
  invoking one — already has it out), or seeds a DETACHED worktree
  populated from a resolved commit-ish; `--detach` forces detachment for
  branch targets (the branch stays free); `-b <new> [<start>]` creates
  and checks out a new branch with full rollback on any later failure
  (no branch-only or orphan-registry residue), refusing an existing name.
  A nonexistent branch fails closed — Git's remote-branch DWIM,
  `worktree.guessRemote`, `--track`/`--no-track`, `-B`/`--force`,
  `--lock`, `--orphan`, and `--no-checkout` are deferred and declared in
  COMPATIBILITY.md. The no-target default (detached at the source
  commit, intentionally different from Git's basename-branch default)
  is unchanged. Re-attach and already-registered paths refuse checkout
  arguments instead of silently ignoring them.

- **Worktree lifecycle: detach instead of drop, tombstones, and a durable
  intent journal (v0.19.58, plan-20260714 Part C §C.7, W3 slice 1b)**:
  `worktree remove` (keep-dir) now DETACHES — the registry entry moves to
  `detached_from_registry`, the worktree's scoped DB rows are preserved
  (previously they were GC'd, leaving a directory that still operated but
  lost its HEAD), and a gitdir marker fail-closes every command inside the
  directory with a re-add/delete hint. `worktree add <path>` re-attaches a
  detached worktree after verifying its gitdir identity against the
  registry's persisted id. `--delete-dir` deletes + fsyncs the parent
  BEFORE cleaning scoped rows; a cleanup failure keeps a `tombstone` entry
  that `worktree repair` retries. Both modes refuse while a
  rebase/cherry-pick/bisect is in progress; `prune` treats only a NotFound
  stat as missing (permission errors never classify a worktree as
  missing) and skips tombstones and active-sequencer scopes. add, move,
  remove, and prune record a durable intent-journal row (migration
  2026072402) before mutating; `worktree repair` rolls interrupted
  operations forward or back deterministically (never deleting
  directories), and the migration's down path refuses while any
  detached/tombstone/journal state exists. `worktree list` reports each
  entry's lifecycle state.

- **Worktree registry v2 with persisted identities and `worktree repair
  <path>` (v0.19.57, plan-20260714 Part C §C.7, W3 slice 1a)**:
  `worktrees.json` moves to `{schema_version: 2, entries: [...]}` and each
  linked entry now PERSISTS its stable `worktree_id`. A legacy v1 file is
  upgraded in place under the registry lock by the first MUTATING worktree
  command (ids backfilled from each worktree's gitdir, all v1 fields
  preserved; lockless readers like `worktree list` never rewrite it);
  every worktree command applies pending migrations before touching the
  registry, so the 2026072401 capability marker refuses pre-v2 binaries at
  connect time before they can misread or rewrite the v2 file, and the
  renamed top-level key makes a v1 parser fail closed as a second belt.
  Parsing discriminates on the top-level shape and validates the v2
  identity invariants (main has no id, linked entries must have one), so
  hybrid/malformed files are refused instead of reinterpreted.
  `worktree repair <path>` restores a linked worktree's missing or corrupt
  `.libra/worktree_id` and `commondir` from the registry's persisted id —
  never a guess — so the worktree maps back to its own scoped
  HEAD/index/stash state; a commondir validly pointing at a different
  storage is refused without touching either file, unregistered paths, the
  main worktree, and a still-v1 registry (no persisted identities — run the
  no-arg repair once to upgrade) are refused, `worktree list` prefers the
  persisted id,
  and the identity-corruption hints now point at the path form.

- **gc/repack run in multi-worktree repositories via the typed GC root
  inventory (v0.19.56, plan-20260714 Part C §C.4.3, W2 final slice)**: the
  versioned `GC_OBJECT_SOURCE_INVENTORY` accounts for every persistent
  OID-bearing store as a traced reachability root or a documented non-root,
  a schema-scan guard test fails any future OID column that ships
  un-inventoried, and the W0 multi-worktree prune/repack skips are lifted.
  New root classes fix real data-loss holes that also affected
  single-worktree repositories: note blobs (`notes.blob` is their only
  anchor), undo view snapshots (`operation_view_ref.target_oid`), AI capture
  checkpoints (`agent_checkpoint` OIDs), in-progress merge/revert/rebase-aux
  sidecar OIDs, and per-worktree `FETCH_HEAD` tips are now kept alive by
  `maintenance run` gc/repack. Unreadable roots still fail the walk closed —
  pruning never proceeds against a partial root set.
- **`merge-file` backups are worktree-local (v0.19.55, plan-20260714 Part C
  §C.4.3, W2 slice 3)**: the in-place backup of `<current>` moves from the
  shared `.libra/merge-file-backup/` into the acting worktree's local gitdir
  — two worktrees merging same-named files no longer overwrite or clean up
  each other's conflict backups (main's location is unchanged since its
  local gitdir IS `.libra`).
- **The stash stack protocol is worktree-aware (v0.19.54, plan-20260714
  Part C §C.4.3, W2 slice 2)**: the stack (`refs/stash` + reflog) stays
  deliberately repository-shared — an entry pushed in one worktree lists,
  applies, and pops from any other — while `push`/`apply`/`pop` snapshot and
  mutate ONLY the acting worktree's own index/workdir (fixing the former
  common-storage index/parent-workdir reads that made a linked `stash push`
  report "No local changes to save"). Stash object reads now go through the
  storage-backed loader (loose AND packed): previously every stash flow read
  loose objects only, so a HEAD that arrived via clone/pull (in a pack) made
  `stash push` silently no-op — an unreadable HEAD commit now reports
  "changed" (fail-safe) instead of masquerading as a clean tree. `pull
  --rebase --autostash` pops EXACTLY the entry it pushed (located by commit
  id, applied by hash) — a shared-stack top that moved concurrently can no
  longer make it apply and delete another worktree's stash. Every stack mutation serializes on a
  cross-platform `stash-stack.lock`; `pop` and `stash branch` delete their
  applied entry through the single by-id CAS `do_drop` (a concurrent stack
  change keeps the entry and reports — never deletes the wrong entry, never
  rolls back the successful apply); `stash branch` preflights the
  branch-name collision and switches HEAD via the fallible API. The W0
  linked-worktree guards on `stash` and `pull --rebase --autostash` are
  lifted — no command remains refused in a linked worktree on
  repository-global-state grounds.
- **rerere `MERGE_RR` is worktree-local (v0.19.53, plan-20260714 Part C
  §C.4.3, W2 slice 1)**: the currently-tracked-conflicts list moves from the
  shared `.libra/rerere/MERGE_RR` into each worktree's local gitdir
  (`<local_gitdir>/MERGE_RR`), so one worktree's `rerere clear`/auto-update
  can no longer drop, stage, or record another worktree's current
  conflicts; the reusable resolution cache
  (`.libra/rerere/<id>/{preimage,postimage}`) stays repository-shared — a
  resolution recorded in one worktree still replays in any other. A legacy
  shared `MERGE_RR` follows the ambiguous-sidecar rules: a linked scope
  never reads it, a single-worktree main reads it and migrates it on first
  write, and once linked worktrees exist it is ignored with a notice and
  left untouched for the worktree doctor (W3).
- **The sparse view is worktree-scoped (v0.19.52, plan-20260714 Part C
  §C.4.1.1, W1 advisory slice 3)**: migration `2026072304` re-keys
  `sparse_view` to UNIQUE(worktree_id, ordinal) and re-projects the
  scope-less `sparse.enabled` config key into the per-worktree
  `sparse_view_meta` table (the config key is retired). Every `sparse-view`
  subcommand and the `ls-files`/`diff`/`status`-advisory/`hydrate` gates act
  on the current worktree's own patterns and toggle; `hydrate` (a
  materialization path) now FAILS CLOSED on an unreadable view instead of
  degrading to "everything in view". Legacy state adopts to main only when
  no linked worktree exists; otherwise the migration fails closed (CHECK
  guard) pending an explicit `sparse-view clear` — patterns are never copied
  to every worktree. The down migration fails closed on linked rows and
  re-projects the main toggle back into config_kv. `worktree remove
  --delete-dir`/`prune` GC sparse rows under the same directory-gone rule as
  layer rows, and the linked-worktree guard on `sparse-view` is lifted.
- **The layer overlay registry is worktree-scoped (v0.19.51, plan-20260714
  Part C §C.4.1.1, W1 advisory slice 2)**: migration `2026072303` re-keys
  `layer` to UNIQUE(worktree_id, name) and `layer_path` to
  UNIQUE(worktree_id, path); every `LayerStore` method, `apply`/`unapply`,
  the `add` staging guard, and the sync ignore-exclusion snapshot carry the
  request's one resolved `WorktreeScope`. All `layer` subcommands now run in
  linked worktrees against their own registrations — the same layer name and
  destination can exist independently per worktree, and `worktree remove`
  purges the removed scope's rows when the directory is deleted too (a
  retained directory keeps its ownership rows so the still-materialized
  overlay files stay un-stageable). Layer ownership is not rebuildable, so
  legacy repository-global rows adopt to main only when no linked worktree
  exists; otherwise the migration FAILS CLOSED (CHECK guard) and asks for an
  explicit `layer unapply`/`layer remove` from the owning worktree. The down
  migration equally fails closed while linked-scope rows exist.
- **The dirty-set cache is worktree-scoped (v0.19.50, plan-20260714 Part C
  §C.4.1.1, W1 advisory slice 1)**: migration `2026072302` re-keys
  `working_dirty` to UNIQUE(worktree_id, path, kind) and
  `working_dirty_meta` from the repository `id = 1` singleton to one
  freshness row per worktree; every `DirtyCache` query/write carries the
  scope predicate. `libra dirty` and `status --scan/--cached/--check-dirty`
  now run in linked worktrees against their own rows — a linked scan can no
  longer read, invalidate, or prune the main worktree's snapshot (and vice
  versa). Legacy rows are cleared by the migration (rebuildable advisory
  state per the plan's owner-never-guessed rule; each scope rescans once);
  the rollback fails closed while linked-scope rows exist. The W0
  entry-guards on `dirty` and the status cache modes are lifted; `stash`/
  `layer`/`sparse-view` stay guarded until their own scoping slices.

- **`status` argv normalization: Git raw `--find-renames` grammar, true
  last-one-wins, `--null`, bare `-z` → porcelain v1 (v0.19.49, plan-20260714
  Part B §B.4.3, R0-4 first slice)**: a pre-clap normalizer (driven by the
  root command's global-arg metadata, never a hand-written flag list)
  rewrites only the `status`/`st` argument slice: raw score values (`505` =
  50.5%, `100%` = exact-only, decimals) ride an occurrence list so the LAST
  of `--no-renames`/`--renames`/`--find-renames[=N]` wins in argv order —
  `--no-renames --find-renames=80` now re-enables at 80% like Git. `-z`
  gains the `--null` alias (conflicting with `--long` and the cache modes),
  and a bare `-z`/`--null` with no explicit format forces porcelain v1
  instead of NUL-terminating the human format. Other commands' argv is
  never touched (`diff status --find-renames=505` stays a pathspec).

- **`status` dirty-cache degradations join the structured warning schema
  (v0.19.48, plan-20260714 Part B §B.5, R0-8b)**: the three legacy
  stderr-only cache warnings — stale-lock steal, stale-cache fallback, and
  concurrent index/HEAD invalidation — now emit
  `dirty_cache_lock_stolen`/`dirty_cache_stale_fallback`/
  `dirty_cache_concurrent_invalidate` with source `cache`: JSON cache modes
  carry them in `data.warnings[]` with a clean stderr, human modes keep the
  stderr line via the shared delivery, and the 9-over-1 exit arbitration
  covers them natively.

- **`status` structured warnings + warning-over-dirty exit arbitration
  (v0.19.47, plan-20260714 Part B §B.5, R0-8a)**: rename-engine degradations
  are no longer silently dropped — `rename_limit_product_skipped` and
  `similarity_budget_exceeded` surface as structured `StatusWarning`s
  (snake_case-pinned `code`/`source`). Human/short/porcelain print them as
  `warning: …` on stderr (even under `--quiet`); `--json` carries them in a
  new top-level `data.warnings[]` and never touches stderr. Every
  `--exit-code` return point now arbitrates locally: with the global
  `--exit-code-on-warning`, warnings exit 9 and beat the dirty exit 1 —
  previously the early `silent_exit(1)` preempted the top-level exit-9 pass
  entirely. Guards: `json_warnings_schema_snapshot`,
  `rename_limit_warning_exit_nine_over_dirty`. Remaining for R0-8b:
  dirty-cache warning mapping onto the same schema plus fault injection.

- **`status` unstaged rename detection now matches Git's default; `RM`/`RD`
  combined records (v0.19.46, plan-20260714 Part B §B.3.1)**: every unstaged
  "new" path is by definition untracked, and Git never consumes untracked
  files as rename destinations — Libra now only pairs them under the new
  config-only extension `status.renameUntracked` (strict boolean cascade,
  default `false`, invalid values fail closed before output). By default a
  tracked→untracked move renders as `D` + `??` instead of a fabricated
  unstaged rename. Additionally, a staged rename whose destination is then
  modified or deleted in the worktree now carries that state in the record's
  Y column (`RM`/`RD` in short/porcelain-v1/v2) — previously the short
  format dropped even the `M`, and a worktree deletion of the destination
  vanished from machine output entirely because the endpoint row was
  suppressed without merging its state. New wave-0 guards:
  `chain_rename_default_untracked_d_and_question`,
  `rename_untracked_config_cascade`, `staged_rename_then_modify_emits_rm`;
  existing unstaged-rename tests now opt in explicitly, and the case-only
  `mv` JSON assertion is updated for default-on rename folding.

- **PD-06 decision closed: local-Libra shallow negotiation stays declined
  (v0.19.45, plan-20260714 Part D)**: the last open decision gate from the
  plan-20260708 migration is resolved — a local Libra source will keep
  failing closed on `--depth` (`LBR-REPO-002` before object transfer, the
  P0-03 end state) instead of growing `shallow <oid>` negotiation. Demand
  is near zero (a local Libra source is directly readable in full) while a
  correct implementation would need boundary generation, `.libra/shallow`
  bookkeeping, deepen/unshallow round-trips, and GC-boundary coupling.
  Recorded as register entry **D20** with rationale and restart conditions;
  `COMPATIBILITY.md`'s Shallow Transfer Integrity section now states the
  fail-closed behavior is the accepted end state. Guard unchanged:
  `compat_clone_shallow_integrity` (re-run green). With this, Part D has no
  undecided items left.

- **Sequencer-family DB scope hardening: migration claim-first + formal
  `bisect_state` re-key (v0.19.44, plan-20260714 Part C §C.4.2, W1
  close-out)**: two audit-found defects are fixed. (1) The
  `sequencer_worktree_scope` (2026071901) down migration silently dropped
  linked-worktree sequence rows on rollback; it now FAILS CLOSED with the
  same CHECK-guard pattern as 2026072101. (2) `bisect_state` graduates from
  lazy `ADD COLUMN` scoping to a formal migration (`2026072301`): re-keyed
  to `worktree_id TEXT PRIMARY KEY` (newest row per scope wins — linked
  rows shipped since v0.19.34 are preserved), down fails closed on linked
  rows, and the lazy DDL in `command/bisect.rs` is deleted (`save` becomes
  a single-statement upsert on the scope key, removing a concurrent-save
  UNIQUE-violation window). Systemically, `MigrationRunner` now claims the
  `schema_versions` row BEFORE running up-DDL inside the same transaction
  (claim-first): a concurrent upgrader that loses the claim skips the DDL
  entirely, which is what makes non-idempotent RENAME-based rebuilds
  (2026072101/2026072301) safe under races — previously the loser re-ran
  them against already-rebuilt tables and errored. A deterministic
  two-racer regression rides on a new `#[doc(hidden)]` post-read gate seam.

- **PD-08 decision closed: no Git hooks bridge (v0.19.43, plan-20260714
  Part D)**: the opt-in `hooks.gitCompatibility` bridge (plan-20260708 P1-10
  Option B) was evaluated and **declined** — Libra keeps never reading
  `.git/hooks` or `core.hooksPath`, and the config key stays an inert,
  unimplemented setting. Decision recorded in the compatibility register
  (D3) and both EN/zh `repository-hooks.md`; a new guard
  (`compat_libra_hooks_lifecycle::git_hooks_bridge_stays_inert_with_gitcompatibility_config`)
  pins that `.git/hooks`, `core.hooksPath`, and the key never execute hooks
  while the sandboxed `.libra/hooks` machinery stays active. Also closes
  PD-00: version strings are re-synced across **four** surfaces
  (`Cargo.toml`, `web/package.json`, `worker/package.json`, and the
  `install.sh` `DEFAULT_VERSION` fallback — the fourth surface found by the
  release-path sweep), and a stale-on-Linux `unnecessary_cast` clippy break
  from the portable `st_mode` widening is annotated in
  `internal::upgrade::lock`.

- **`rebase` (and `pull --rebase`) now run in linked worktrees (v0.19.42,
  plan-20260714 Part C §C.4.2 — the final W1 sequencer lift)**: with
  `rebase_state` keyed per-worktree, the aux sidecar in the worktree-local
  gitdir, a scope-aware sequencer mutex, per-scope GC reachability roots, and
  a worktree-scoped operation dedup window all in place, the blanket
  linked-worktree refusal is lifted. Two worktrees can rebase their own
  branches concurrently; a conflicted rebase stopped in one worktree never
  blocks (and cannot be continued/aborted from) another. Branch-ref finish
  safety is unchanged: `--update-refs` excludes branches checked out in any
  worktree and the finish compare-and-swap detects concurrent tip movement.
  Only `pull --rebase --autostash` remains refused in a linked worktree — its
  legacy wrap uses the repository-global stash stack (main-only until W2).

- **Internal: the operation duplicate-submission window is scoped
  per-worktree (v0.19.41, plan-20260714 Part C §C.9, W1 slice 3b)**: the
  `operation` audit table gains a `worktree_id` column (migration
  `2026072201`; main = `""`), the in-process active-key set embeds the scope,
  and the 5s duplicate window only consults THIS worktree's history — the
  same command with identical arguments run concurrently in two worktrees is
  two legitimate operations, not a duplicate submission. The down migration
  preserves every audit row (only the scope attribution is dropped, which the
  old schema cannot represent).

- **Captured agent sessions now have a privacy-preserving read-only graph**:
  `libra agent graph <session>` renders turn, revision, checkpoint, and
  subagent-link structure in a two-pane TUI, with frozen JSON schema v1 for
  automation. The projection never reads transcript/object blobs or sensitive
  metadata columns, preserves shared checkpoint evidence, distinguishes
  legacy `unindexed`, local `erased`, and unknown sessions, and refuses a TUI
  before initialization when stdin/stdout are not terminals.

- **Terminal local `object_index` failures are durable and repairable**: every
  queued object-index row now has an atomic repair marker until the SQLite row
  is reconciled. Ordinary object-writing commands keep their completed local
  result and emit a warning instead of producing contradictory success/error
  JSON; the next schema-aware repository command retries the exact rows, while
  `cloud sync` fails closed before network work if repair is still impossible.
  Public in-process CLI invocations are serialized, while task-local failure
  and pending-work scopes keep concurrent direct storage callers out of the
  active command's warning and drain accounting. Invocation-scoped updates
  use a separate bounded FIFO lane, so an earlier direct-library backlog also
  cannot consume the command's finite drain budget. Command-owned spawned
  producers register before they start and explicitly inherit the invocation
  scope, so the drain cannot observe a transient zero before they enqueue;
  every queued message retains its originating invocation's scope even if it
  completes after that invocation's drain budget. Terminal index failures are
  also warned when an earlier input was persisted before a later input made the
  primary command fail; the original command error and exit code remain
  authoritative. Queue drain is an
  async, 60-second bounded wait so embedded Tokio executors remain responsive.
  Replay uses one database connection, enumerates at most 100,000 raw repair
  directory entries per page, and owns at most one 100-row database batch per
  invocation; oversized queues make progress across later commands instead of
  becoming permanently unreplayable. Replay and queued writers share a
  process-crash-safe ownership lock from a bounded 65,536-shard OID namespace
  through the row update and
  marker retirement, so a delayed writer cannot recreate an index row after
  destructive cleanup. Marker publishers additionally share a repository-wide
  generation fence with destructive cleanup; cleanup revalidates candidate
  OIDs under that fence and holds it through the SQLite prune commit, closing
  the marker-creation window after command preflight. Foreground lock acquisition has a two-second deadline
  and returns an actionable retry error instead of hanging behind a stalled
  process; replay releases object ownership after every database batch;
  concurrently retired markers are treated as completed work. Cloud operations and
  destructive `agent clean` runs fail closed while another page remains or a
  canonical final marker is malformed. New atomic writes use a separate
  staging directory; bounded replay scavenges legacy `.tmp*` remnants from the
  final directory and removes at most 256 staging files older than 24 hours per
  1,024-entry scan, so crash debris cannot starve markers or leak indefinitely.
  Marker OIDs must match the repository's configured SHA-1/SHA-256 format. A marker
  created after a successful configured-backend write remains valid when the
  payload is remote-only. With `--sync-data`, successful marker unlink also
  fsyncs the marker directory so a power loss cannot restore retired work.
  Marker creation and retirement failures are surfaced
  through the owning command's error/warning contract; `add` and `update-index`
  now return `LBR-IO-002` instead of panicking if marker registration fails,
  and a normal retry re-registers an already-persisted payload instead of
  silently skipping its cloud index row. The public `BlobExt::save` API is now
  fallible and returns `io::Result<ObjectHash>`; `try_save` remains as a
  fallible compatibility alias, and command paths propagate both with
  actionable context instead of terminating a library consumer.
  Agent import retains its stricter `LBR-AGENT-018` durable barrier contract.

- **GC: in-progress sequencer / rebase / bisect state rows are now
  reachability roots (v0.19.39, plan-20260714 Part C §C.9 item 10)**: an
  interrupted cherry-pick's todo commits, a stopped rebase's
  `onto`/`orig_head`/`current_head`/`todo`/`done`/`stopped_sha`, and a bisect
  session's `orig_head`/`bad`/`good`/`current`/`skipped` previously had NO
  anchor in the reachability walk once refs and reflogs moved on — one
  maintenance run could delete the very objects `--continue` needs.
  `collect_reachable_objects` now traces those OID columns across EVERY
  worktree scope (fail-closed: an unreadable row or invalid OID aborts rather
  than pruning against a partial root set; the free-form sequencer `payload`
  is scanned leniently). Held merge/rebase autostash sidecars are likewise
  enumerated across ALL worktrees' gitdirs, not just the one gc runs from.

- **Internal: the sequencer mutex probes rebase per-worktree (v0.19.38,
  plan-20260714 Part C §C.4.4, W1 rebase slice 3/4)**:
  `detect_active_operation` now probes THIS worktree's scoped `rebase_state`
  row before the linked-worktree early-return, so once the rebase guard lifts,
  a linked worktree's rebase occupies its own mutex while never blocking (or
  being blocked by) another worktree's sequence. The legacy
  `rebase-merge`/`rebase-apply` directory probes and the pre-2.6
  `cherry_pick_state` probe remain main-only (ambiguous-sidecar rule).
  No behavior change until the guard lifts.

- **Internal: rebase sidecars become worktree-local; the legacy
  `rebase-merge/` directory is no longer auto-adopted on ambiguous ownership
  (v0.19.37, plan-20260714 Part C §C.4.2, W1 rebase slice 2/4)**:
  `rebase-aux.json` (exec queue, update-refs plan, rewrites, held autostash
  oid) now lives in the worktree-LOCAL gitdir — unchanged paths for the main
  worktree, per-worktree state once rebase is allowed in linked worktrees.
  The legacy common `.libra/rebase-merge/` crash-state directory is never
  consumed from a linked worktree, and the main worktree refuses to adopt it
  while linked worktrees are registered (its owner would be ambiguous, and
  adoption destroys the directory) — the error explains how to resolve it.
  A linked worktree's `rebase` cleanup also no longer deletes the common
  legacy directory. Behavior in single-worktree repositories is unchanged.

- **Internal: `rebase_state` is re-keyed per-worktree and its lazy DDL is
  retired (v0.19.36, plan-20260714 Part C §C.4.2, W1 rebase slice 1/4)**:
  migration `2026072101_rebase_state_worktree_scope` rebuilds the table to
  `worktree_id TEXT PRIMARY KEY NOT NULL` (main = `""`, the sequencer
  convention), migrating the newest in-progress row to the main scope. The
  historical lazy ADD-COLUMN DDL produced databases with any subset of
  `autosquash`/`todo_actions`/`empty_mode`, so a `normalize_rebase_state_shape`
  hook runs before the migration runner on every connection open and fills the
  missing columns first; the lazy DDL in `command/rebase.rs` is deleted and
  the schema is now owned by the migration. Every runtime statement is scoped
  `WHERE worktree_id = ?` — no more unconditional `DELETE FROM rebase_state` —
  and `worktree remove` purges the removed worktree's row. The down migration
  FAILS CLOSED while a linked worktree's rebase row exists. Behavior is
  unchanged for now: `rebase` itself is still refused in a linked worktree
  until the sidecar/mutex slices land.

- **`pull` (merge/ff mode) now runs in linked worktrees (v0.19.35,
  plan-20260714 Part C §C.4.4)**: pull's fetch phase writes only
  repository-scoped state (`refs/remotes/*` + objects — it writes no
  FETCH_HEAD; the public `fetch` command's FETCH_HEAD has been worktree-local
  since v0.19.29) and its merge phase runs on the fully worktree-scoped merge
  state (since v0.19.33), so the blanket linked-worktree refusal is lifted. Only the REBASE mode still fails closed
  there — its `rebase_state` (and the legacy stash-stack autostash it uses)
  is still repository-global — and the mode is resolved AFTER
  `pull.rebase`/`branch.<name>.rebase` config, before any fetch, so an
  implicitly configured rebase cannot slip past the guard.

- **`bisect` now runs in linked worktrees (v0.19.34, plan-20260714 Part C
  §C.4.2)**: the `bisect_state` row is keyed by `worktree_id` (main worktree =
  `""`, matching the sequencer convention), so each worktree's session —
  start/good/bad/skip/run/log/view/reset — is fully independent and two
  worktrees can bisect concurrently without interfering. Of the
  sequencer-family ops, only `rebase` remains refused in a linked worktree
  (its state is still repository-global). Four correctness fixes ride along:
  - *Checkout rewrites the index in step with the worktree*: bisect checkouts
    now go through the canonical restore contract (the same call `switch`
    makes), rewriting the per-worktree index AND working tree together — the
    old burn-down repaint never touched the index, so every bisect step showed
    phantom `status` modifications. Side effect (git parity): untracked files
    created mid-session survive a step instead of being deleted.
  - *Linked-worktree checkout targets the right tree*: the old repaint
    resolved the working directory via the shared storage path's parent — the
    MAIN worktree's directory — so a linked worktree's bisect would have
    materialized candidate trees into the wrong worktree.
  - *`bisect reset` honors branch exclusivity*: while a worktree bisects
    (detached), another worktree may legitimately check out its original
    branch; reset now warns and ends the session detached instead of silently
    attaching one branch to two HEADs (the state `switch`/`checkout` refuse).
  - *`worktree remove` GCs the scoped session rows*: worktree ids are
    deterministic (hash of the canonical path), so a stale `bisect_state`/
    `sequence_state` row would be inherited — and a dead bisect session
    silently resumed — by a worktree re-added at the same path.

- **Internal: `WorktreeScope` is now the single worktree-scope value object
  (v0.19.24, plan-20260714 Part C §C.4.1)**: scope resolution no longer passes a
  bare `Option<String>` around for each layer to reinterpret. The type encodes
  both storage conventions explicitly — `worktree_id()` for the `reference`
  (HEAD) table, where the main worktree is spelled `NULL`, and `storage_key()`
  for `worktree_id TEXT NOT NULL` columns, where main is the empty string (a
  nullable unique key cannot express "at most one row per scope" in SQLite).
  A linked worktree can never alias onto main in either form. The HEAD scope
  query and the linked-worktree guards now resolve through it; behavior is
  unchanged.

- **`fast-import` refuses a branch checked out in another worktree (v0.19.22,
  plan-20260714 Part C W0 §C.11)**: the batch ref flush rewrites and deletes
  shared branch refs; it now fails closed, before the transaction, if any
  target branch is checked out in a different worktree. Importing into this
  worktree's own branch is unaffected. This completes the cross-worktree
  ref-writer guard set (`branch`/`update-ref`/`symbolic-ref`/`op restore`/
  `reflog expire --updateref`/`checkout`/`switch`/`fast-import`); `fetch`
  already refused checked-out destinations across all worktrees.

- **`reflog expire --updateref` refuses a branch checked out in another
  worktree (v0.19.21, plan-20260714 Part C W0 §C.11)**: `--updateref` moves a
  pruned branch's tip to its newest surviving reflog entry; it now fails closed,
  before any write, when a target branch is checked out in a different worktree
  (moving its tip would diverge that worktree's working tree). Plain reflog
  expiry (no `--updateref`) only trims entries and is unaffected.

- **`op restore` refuses a branch checked out in another worktree (v0.19.20,
  plan-20260714 Part C W0 §C.11)**: `op restore` rewrites and prunes shared
  branch refs to reproduce a past operation's view; it now fails closed, before
  any write, if any branch it would move or prune is checked out in a different
  worktree (that worktree's HEAD would dangle). Restoring this worktree's own
  branch is still allowed.

- **`checkout`/`switch --ignore-other-worktrees` no longer bypasses the
  same-branch guard (v0.19.19, plan-20260714 Part C W0 §C.11,
  intentionally-different from Git)**: Libra never allows the same shared branch
  checked out in two worktrees, so `--ignore-other-worktrees` is now accepted
  only for CLI compatibility — it does NOT override the refusal in a
  multi-worktree repo (against a real collision the checkout is still refused,
  with a note that the flag is not honored). It remains a silent no-op in a
  single-worktree repo (no collision to override). Docs, `COMPATIBILITY.md`, and
  the error hint (which no longer suggests the flag) are updated accordingly.

- **`symbolic-ref HEAD` refuses a branch checked out in another worktree
  (v0.19.18, plan-20260714 Part C W0 §C.11)**: `symbolic-ref HEAD
  refs/heads/<branch>` now fails closed when `<branch>` is already checked out
  in a different worktree, preventing a forbidden duplicate checkout (the same
  guard `switch`/`checkout` already apply). Re-pointing at this worktree's own
  current branch is still allowed.

- **`update-ref` refuses to move/delete a branch checked out in another
  worktree (v0.19.17, plan-20260714 Part C W0 §C.11)**: `update-ref
  refs/heads/<branch>` now fails closed when `<branch>` is checked out in a
  different worktree (its HEAD would dangle or its working tree diverge),
  joining the `branch -d`/`-m`/`reset` guards. Updating this worktree's own
  current branch is still allowed.

- **Destructive branch writers refuse a branch checked out in another worktree
  (v0.19.16, plan-20260714 Part C W0 §C.11)**: `branch -d`/`-D` (delete),
  `branch -m`/`-M` (rename), and `branch reset` now fail closed when the target
  branch is checked out in a DIFFERENT worktree, instead of leaving that
  worktree's HEAD dangling (delete/rename) or silently diverging its working
  tree from its branch (reset) — matching Git, which refuses these across
  worktrees. The current worktree's own branch is still caught by the existing
  "currently on"/"reset current branch" checks, and a branch checked out
  nowhere else remains freely mutable.

- **`status --scan`/`--cached`/`--check-dirty` fail closed in a linked worktree
  (v0.19.15, plan-20260714 Part C W0)**: these dirty-cache modes read/prune the
  repository-global `working_dirty`/`working_dirty_meta`, so they now refuse to
  run in a linked worktree until W1 scopes the cache. Plain `status` (and
  `status --porcelain`/`--short`) is unaffected — it never consults the dirty
  cache, so it already computes a fresh, correct result in any worktree.

- **Repository-global-state commands fail closed in a linked worktree
  (v0.19.14, plan-20260714 Part C W0 transition guards)**: `stash` (all
  subcommands, incl. `stash branch`), `layer`, `sparse-view`, `dirty`, and the
  composite `fetch`/`pull` now refuse to run inside a linked worktree with an
  actionable "run it in the main worktree" error, joining the existing
  merge/rebase/cherry-pick/revert/bisect/am refusal. Their stores (the stash
  stack, dirty cache, layer/sparse tables, shared `FETCH_HEAD`) are still
  repository-global, so a linked invocation could read or clobber the wrong
  worktree's state; the guard fires before any side effect. The main worktree
  is unaffected. Each guard is lifted per-command as W1/W2 make that store
  worktree-scoped.

- **`rev-parse --git-dir`/`--absolute-git-dir`/`--is-inside-git-dir` return the
  current worktree's local gitdir (v0.19.13, plan-20260714 Part C W0 §C.5)**:
  these queries now resolve (and test) THIS worktree's own `.libra` rather than
  the shared common storage. For the main worktree the result is unchanged
  (local == common); for a linked worktree `--git-dir` now points at its
  private `.libra` (holding its own HEAD/index), so scripts that locate the
  index/EDITMSG via `--git-dir` hit the correct per-worktree gitdir and
  `--is-inside-git-dir` no longer misreports a cwd inside the linked `.libra`.

- **`for-each-ref %(worktreepath)` resolves across worktrees (v0.19.10,
  plan-20260714 Part C W0 §C.3.3)**: the atom now reports the path of the
  worktree that actually has each branch checked out — resolved across ALL
  registered worktrees from each worktree's own scoped HEAD row — instead of
  assuming a single shared HEAD and always returning the current worktree. A
  branch checked out in a linked worktree reports that worktree's path even
  when `for-each-ref` runs elsewhere; a branch checked out nowhere (or a
  detached worktree) is empty. Single-worktree output is unchanged.

- **`worktree list --porcelain` reports each worktree's own HEAD (v0.19.9,
  plan-20260714 Part C W0 §C.3.3)**: in the isolated worktree layout each
  entry now emits its OWN `HEAD <sha>` plus a `branch <ref>` or `detached`
  line (resolved from that worktree's scoped HEAD row via
  `Head::head_for_worktree_scope`), instead of stamping the running command's
  HEAD onto every entry. An entry whose HEAD cannot be resolved (a legacy
  shared-`.libra` symlink layout, or a missing/corrupt scope) omits the HEAD
  lines rather than being mislabeled with another worktree's commit. The
  `worktree list` JSON/entry now carries a stable `worktree_id`. Corrects the
  worktree/architecture docs and `COMPATIBILITY.md` (which had described a
  shared HEAD and `--delete-dir`-gated scoped-row GC) to the isolated reality.

### Changed

- **`merge` is now allowed in a linked worktree (v0.19.33, plan-20260714
  Part C W1 §C.4.2/§C.4.3)**: `merge`'s in-progress state (`merge-state.json`)
  and its held autostash (`merge-autostash.json` — still a fail-closed GC root,
  protected in a multi-worktree repo by GC's per-repo prune skip) now live in
  the invoking worktree's own gitdir, and the sequencer mutex probes that
  worktree-local merge state. So a merge in one worktree neither collides with
  nor is blocked by another's, and it merges into that worktree's own branch.
  The `ensure_main_worktree` refusal is lifted. `pull` remains refused in a
  linked worktree (it drives merge through a not-yet-scoped internal path), and
  rebase/bisect remain refused. This completes the linked-worktree lift for
  every sequencer op except rebase and bisect.

- **`revert` is now allowed in a linked worktree (v0.19.32, plan-20260714
  Part C W1 §C.4.2)**: `revert`'s in-progress state (`revert-state.json`) and its
  editor buffer (`REVERT_EDITMSG`, moved in v0.19.28) now live in the invoking
  worktree's own gitdir, and the start-time sequencer mutex probes that
  worktree-local revert state — so a revert in one worktree neither collides
  with nor is blocked by another's. The `ensure_main_worktree` refusal is lifted.
  (For the main worktree the local gitdir is the common storage, so an
  in-progress revert started by an older binary is still found after upgrade.)
  merge/rebase/bisect remain refused.

- **`am` is now allowed in a linked worktree (v0.19.31, plan-20260714 Part C W1
  §C.4.2)**: like `cherry-pick`, `am`'s entire persistent state is the
  worktree-scoped `sequence_state` row (the patch queue is serialized into its
  `payload`; there is no common-storage sidecar), so applying a mail series in a
  linked worktree lands on that worktree's own branch without touching another's
  state. The `ensure_main_worktree` refusal is lifted. merge/rebase/revert/bisect
  remain refused.

- **`cherry-pick` is now allowed in a linked worktree (v0.19.30, plan-20260714
  Part C W1 §C.4.2)**: with its state fully worktree-scoped — the
  `sequence_state` row keyed by `worktree_id` (v0.19.26) and the local-gitdir
  `CHERRY_PICK_MSG` (v0.19.28) — the `ensure_main_worktree` refusal is lifted.
  The start-time sequencer mutex is now scope-aware too: in a linked worktree it
  only considers that worktree's own scoped sequence, not another worktree's,
  and not the main-only merge/rebase/revert state (which can never be active for
  a linked worktree). Two worktrees can cherry-pick onto their own branches
  concurrently without their sequencer state or message buffer colliding.
  merge/rebase/revert/bisect/am remain refused (their state is still global).

- **`FETCH_HEAD` is worktree-local and `fetch` is allowed in a linked worktree
  (v0.19.29, plan-20260714 Part C W1 §C.4.2)**: `FETCH_HEAD` — the record of the
  refs a worktree just fetched, which `pull` reads back — was written to the
  shared common storage, so a fetch in one worktree overwrote another's record.
  It now lives in the invoking worktree's own gitdir (per Git). With that the
  only per-worktree state fetch touched, the W0 linked-worktree refusal on
  standalone `fetch` is lifted: its other writes (`refs/remotes/*` and the
  object store) are repository-scoped, and fetching into a branch checked out in
  another worktree is still refused. `pull` stays refused in a linked worktree
  because its merge/rebase state is still repository-global.

### Fixed

- **Editor scratch buffers are now per-worktree (v0.19.28, plan-20260714
  Part C §C.4.3)**: `TAG_EDITMSG`, `NOTES_EDITMSG`, `BRANCH_DESCRIPTION_EDITMSG`,
  `CHERRY_PICK_MSG`, and `REVERT_EDITMSG` were written to the shared common
  storage. `tag`/`notes`/`branch --edit-description` are Repository-scope
  commands allowed in any worktree, so two worktrees composing a message at the
  same time would truncate each other's buffer. Each now lives in the invoking
  worktree's own gitdir (identical path for the main worktree, where local and
  common storage are the same directory).

- **The cherry-pick/am/revert sequencer state is now per-worktree (v0.19.26,
  plan-20260714 Part C W1 §C.4.2)**: `sequence_state` was declared
  `id INTEGER PRIMARY KEY CHECK (id = 1)` — one active sequence per
  *repository*. Since `save` is a `DELETE`+`INSERT`, a sequence started in a
  second worktree would overwrite the first worktree's todo list and stopping
  point. Migration `2026071901` re-keys the table on `worktree_id` (main = the
  empty string, deliberately not NULL: SQLite treats every NULL as distinct, so
  a nullable key cannot express "at most one row per scope"), and every
  load/save/clear now carries that key — including `clear`, which previously
  matched on `kind` alone and so could erase another worktree's sequence of the
  same kind. An in-progress sequence survives the upgrade as the main
  worktree's row, and the down-migration restores the single-row shape.
  `rebase_state`/`bisect_state` are deliberately NOT migrated: their column set
  is defined by lazy `CREATE TABLE`/`ADD COLUMN` DDL in the command code, so a
  static rebuild could drop columns it did not know about and destroy an
  in-progress rebase — they stay refused in linked worktrees (hence no
  concurrent writer) until that lazy DDL is retired.

- **Every worktree's index is now a reachability root (v0.19.25,
  plan-20260714 Part C §C.9)**: the reachability walks used by `gc`/`repack`
  and by `fsck` each read only the CURRENT worktree's index, so a blob staged
  in ANOTHER worktree was treated as unreferenced — `fsck --unreachable`
  reported it as garbage, which invites a manual delete. Both walks now collect
  every registered worktree's private index, across all stages (0–3, so a blob
  held only by an unmerged conflict stage counts too). This is the first
  reachability-root source of the per-worktree inventory; `gc`'s multi-worktree
  prune guard stays until the remaining root types (held sidecars, operation-view
  pointers, sequencer rows) are also collected.

- **`gc` no longer prunes objects reachable only from a linked worktree
  (v0.19.23, plan-20260714 Part C W0 release gate §C.11)**: the
  garbage-collection reachability walk reads only the CURRENT worktree's index,
  so a blob staged (but not yet committed) in a linked worktree was not a root —
  running `maintenance run --task gc` from the main worktree could delete it.
  In a repository with linked worktrees `gc` now skips the loose-object prune
  entirely and says so in its task message, instead of deleting objects it
  cannot see. `--dry-run` still previews, and single-worktree repositories are
  unaffected. Pruning is re-enabled there once every worktree's reachability
  roots are collected. The `incremental-repack` maintenance task has the same
  gap — it rebuilds one consolidated pack from the reachable set and then
  deletes the old packs, dropping any object that lived only in an old pack and
  is reachable only from a linked worktree — so it skips in a multi-worktree
  repository too. (Standalone `repack -d` was never affected: it only removes
  loose objects that are now inside the new pack, and never deletes packs.)

- **AI session/MCP storage roots no longer silently mint a phantom `.libra`
  (v0.19.12, plan-20260714 Part C W0 §C.4.1)**: the AI session-transcript store
  now fails closed (returns "no store", with a warning) when storage-root
  resolution fails, instead of rooting itself at a library-less
  `<working_dir>/.libra`. The `code` runtime's `resolve_storage_root` and the
  MCP server's `init_mcp_server` still degrade (they are designed to keep a
  read-only session alive) but now log a loud, diagnosable warning naming the
  fallback and pointing linked-worktree corruption at `libra worktree repair`,
  rather than falling back silently.

- **Linked worktree with a corrupt `commondir` fails closed instead of routing
  a phantom repository (v0.19.11, plan-20260714 Part C W0 §C.4.1)**:
  `worktree_common_storage` previously fell through to treating a linked
  worktree's library-less local `.libra` as the shared storage whenever its
  `commondir` pointer was unreadable or had an empty first line, so every
  db/objects lookup silently targeted a non-existent database inside the
  worktree (a "phantom repo", surfacing as a confusing `LBR-REPO-002` at
  `<wt>/.libra/libra.db`). It now fails closed at path resolution: a missing
  `commondir` still resolves to the gitdir (the main worktree), but a present
  yet corrupt pointer is an error pointing at `libra worktree repair`.

- **`status` no longer reports an unreadable tracked file as deleted (v0.19.7,
  plan-20260714 Part B §B.6.0.1)**: `collect_tracked_worktree_changes`
  previously treated ANY `symlink_metadata` error on a tracked path as a
  deletion, so a permission-denied or I/O error would surface as `deleted:`
  and could make `commit -a` record a spurious removal. Now only a genuine
  `NotFound` counts as a deletion; a real I/O error fails closed with
  `LBR-IO-001` and a hint, rather than inventing a deletion.

### Changed

- **`status --porcelain` (v1) renders renames with Git's arrow form (v0.19.8,
  plan-20260714 Part B R0-6 v1 slice)**: a detected rename in porcelain v1 now
  renders as a single `R  <old> -> <new>` record (`XY SP <new> NUL <old> NUL`
  under `-z`) rather than two `R` endpoint rows, matching Git. This completes
  Git-compatible rename rendering across every `status` output format (human,
  short, porcelain v1/v2, JSON).

- **`status` porcelain v2 and JSON emit proper rename records (v0.19.6,
  plan-20260714 Part B R0-5 + R0-7 JSON)**: `--porcelain=v2` now renders a
  detected rename as Git's single `2 <XY> N... <mH> <mI> <mW> <hH> <hI>
  R<score> <new>\t<old>` record — with the real HEAD tree modes/hashes, index
  modes/hashes, and worktree mode (`<new> NUL <old> NUL` path field under
  `-z`) — instead of two `1 R` change rows for the endpoints. `--json` gains
  a top-level `data.renames[]` array of `{from, to, score, exact, staged,
  unstaged}` (destination-sorted) alongside the existing nested
  `staged.renamed`/`unstaged.renamed` `{from,to}` entries. The similarity
  score is threaded from the diffcore engine through the render pipeline.

- **`status --short` renders renames with Git's arrow form (v0.19.5,
  plan-20260714 Part B R0-6 first slice)**: a detected rename now renders as
  one `R  <old> -> <new>` line (colored `R` in color mode) instead of two
  separate `R` rows for the endpoints; under `-z` the record is Git's
  `XY SP <new> NUL <old> NUL`. Non-rename rows are unchanged, and the legacy
  `generate_short_format_status` public API keeps its pre-existing tuple
  shape. Porcelain v1/v2 rename records land in a follow-up slice.

- **`status.renames` config cascade (v0.19.4, plan-20260714 Part B R0-7)**:
  `libra status` now honors `status.renames` (falling back to `diff.renames`)
  through the strict local → global → system cascade to set the rename-
  detection default — `false` disables it, a truthy or unset value enables it
  at 50%. `copy`/`copies` are rejected (copy detection is unsupported) instead
  of silently degrading, and invalid values fail closed with `LBR-CLI-002`
  before output. CLI flags (`--no-renames`/`--find-renames`/`--renames`)
  always win over config. Documented in `docs/commands/status.md` (+ zh-CN).

- **`libra status` rename detection is now on by default (v0.19.3,
  plan-20260714 Part B R0-2/R0-4)**: a staged or unstaged delete+add pair with
  similar content is reported as a rename without any flag, matching Git's
  default. Matching moves to the shared diffcore engine
  (`command::rename_detect`) — exact by blob id, then unique basename, then a
  bounded inexact spanhash pass with per-side rename limit (1000) and a
  similarity-comparison budget — replacing the previous greedy basename-LCS
  matcher. Detection now runs on repo-relative keys, so renames are found
  correctly when `status` is invoked from a subdirectory. `--no-renames`
  disables it (and wins over `--find-renames`/`--renames`); the dirty-cache
  `--cached`/`--check-dirty` extensions run without rename detection. Staged
  snapshots pair HEAD tree ↔ index stage-0; unstaged pair index stage-0 ↔
  worktree, per Git's content-addressing.

### Added

- **Consented historical agent import and source-scoped subagent capture**:
  adds bounded, redacted Claude Code/Codex/OpenCode transcript backfill with
  durable replay identity, local erasure tombstones, and partial-progress
  reporting. Claude child transcripts are captured as independently versioned
  content checkpoints with fail-closed replay integrity, doctor diagnostics,
  retention-aware cleanup, and cloud mirror/restore companions. Local capture
  cloud mirroring uses explicit session/checkpoint/link/claim revisions, versioned remote
  tables that reject unfenced legacy writers, a token-fenced publication
  manifest bound to the checkpoint-reachable object-index projection, complete D1/R2
  traces/object durability checks, bounded requests, and atomic monotonic
  restore. Existing remote capture
  catalogs without a generation manifest require one current-version sync
  before restore; empty legacy capture layers remain restorable and restore
  never installs remote writer barriers. Adoption removes unrestorable legacy
  checkpoint orphans before strict dependency validation. Prune rewrites advance
  a checkpoint generation so stale clones cannot restore the old traces chain;
  explicit erased-session recovery advances a durable session/source incarnation.
  Object-index reads use generation-fenced keyset pagination, and manifest
  completion atomically verifies that generation plus the fenced traces head;
  full restore reads share a 100,000-row aggregate safety bound across every
  capture catalog table and the fenced object-index projection. Required R2
  payloads are content-verified in fixed 32-object concurrency pages, with
  missing or corrupt objects replaced and read back before manifest completion.
  Crashed `publishing` generations can be atomically recovered after their
  server-timestamped five-minute lease, while active writers retain their fence.
  Local catalog
  transactions are released before the long object walk and rechecked
  afterward, so concurrent capture writers are not held behind the cloud scan.
  Empty checkpoint catalogs reject nonempty traces heads, and prune cleanup
  orders claim/revision/link/checkpoint removal so every interruption boundary
  remains resumable.
  Ordinary checkpoint retention writes durable local/remote prune tombstones so
  stale clones cannot reintroduce deleted checkpoint identities. Restore checks
  local fences before downloading objects and defers the traces ref from
  generic metadata restore until capture ownership is known: a validated
  generation installs its fenced ref, while an empty legacy capture layer
  retains the pre-manifest metadata ref. Ordinary repositories larger than
  100,000 objects remain restorable because the aggregate cap applies only to
  capture catalog/object projections. Historical-import summaries separately
  count parent and child checkpoints, and unavailable child discovery is
  diagnostic rather than a false partial failure. Session
  erasure still does not propagate a deletion tombstone to D1/R2; an unmarked
  remote-only checkpoint therefore stops a new capture generation before
  publication and leaves the previous completed snapshot restorable. A later
  cloud restore can resurrect a remotely mirrored capture; cross-device erase
  propagation remains explicitly deferred.

- **`diff.renameLimit` / `diff.renameComparisonBudget` documentation
  (plan-20260714 R0-1)**: documents the per-side inexact-pass limit and the
  similarity-comparison budget (both non-negative, `0` = unlimited, invalid
  fails closed with `LBR-CLI-002`) in `docs/commands/diff.md` and the zh-CN
  translation.

- **Auto-upgrade integration tests and docs (v0.19.2, plan-20260714 §A.9/
  §A.11)**: two new `test-upgrade`-gated integration targets —
  `upgrade_auto_test` (end-to-end signature+decision chain, revocation-replay
  and same-version-identity anti-rollback, the real-binary `__upgrade-probe`
  self-check across a process boundary, and install/rollback transactions) and
  `upgrade_publish_contract_test` (matrix coverage, URL binding, size bounds,
  channel, and renew-preserves-pause/revocations). Registered with
  `required-features = ["test-upgrade"]`, indexed in `tests/INDEX.md`, and run
  in a dedicated CI step; `release.yml` gains a guard that fails the release if
  the `test-upgrade` feature is ever spliced into a release build. New
  `docs/auto-upgrade.md` plus README and config-doc coverage of supported
  platforms, the official-install requirement, network/throttle behavior, and
  recovery/rollback. The subsystem remains inert until the release-key
  ceremony (see the note below).

- **Auto-upgrade orchestration and startup hooks (v0.19.1, plan-20260714
  §A.7/§A.8/§A.10)**: new `internal::upgrade::orchestrator` wires the whole
  flow. `startup_recovery_gate` runs before repo preflight and drives any
  crashed install transaction to a terminal state (a fatal, unrecoverable
  transaction stops the process before the user's command; a rollback emits
  an advisory). `run_auto_upgrade_check` implements the `upgrade.mode=auto`
  check — throttle gate, signed-manifest fetch, decision, candidate download
  + self-check, and install under the §A.5 lock with the post-install probe —
  and is fully failure-isolated so it can never break or fail the user's
  command (a new `emit_advisory_warning` reports without tripping
  `--exit-code-on-warning`). Both hooks short-circuit with no I/O until the
  compiled trust table is populated, so auto-upgrade is inert by construction
  until the release-key ceremony. A synchronous bounded `run_sync_probe`
  backs the recovery-path self-check. Wired into `cli.rs` startup.

### Note

- The auto-upgrade subsystem (plan-20260714 Part A) is code-complete through
  orchestration but remains **inert**: `PRODUCTION_TRUSTED_KEYS` is empty
  pending the official release-key ceremony, and the signing/publish jobs and
  `install.sh` official-marker path are not yet wired. Until then Libra never
  checks for or installs upgrades regardless of `upgrade.mode`.

- **Auto-upgrade decision pipeline and candidate self-check entry (v0.19.0,
  plan-20260714 §A.7/§A.10)**: new `internal::upgrade::flow` composes the
  pure decision — verify → anti-rollback/time → platform support (Windows
  published-but-unsupported in R0) → `paused`/`revoked`/`newer` gates →
  artifact selection — into a single `Install`/`Skip` verdict carrying the
  marker and anti-rollback state to persist on commit. A new hidden
  front-of-argv `__upgrade-probe --kind <version|pre-install|post-install>
  --expected-version <X.Y.Z>` entry (recognized in `cli.rs` before clap, repo
  preflight, schema migration, config writes and background tasks) runs only
  a side-effect-free identity self-check of the running binary and exits,
  never forwarding to a real command; a malformed or mismatched probe exits
  non-zero silently so the orchestrator fails closed. Because it is
  front-scanned like `help error-codes`, it stays invisible to help, the
  Command-Groups banner and every compat guard. Internal machinery only.

- **Crash-safe install transaction and candidate probes (v0.18.99,
  plan-20260714 §A.7)**: new `internal::upgrade::{txn,probe}`. `txn`
  journals the install to `.libra-upgrade-txn.json` through the seven-state
  machine (Prepared → BackupDurable → CandidateInstalled → PostProbePassed →
  Committed, with RollbackIntent/AbortAbsentIntent branches), always writing
  intent before each filesystem mutation and implementing the full §A.7
  recovery decision table so any crash point resolves idempotently to
  committed, rolled-back-to-previous, or aborted-fresh — the post-probe is
  injected so every intermediate on-disk layout is covered by a direct
  reconstruction test. `probe` spawns the candidate/target self-check in its
  own process group with `kill_on_drop` and a hard per-probe timeout,
  killing and reaping the whole group on timeout so no descendant survives;
  any nonzero exit, signal, timeout or spawn failure is a fail-closed probe
  failure. Internal machinery only.

- **Install-directory lock and official-install marker (v0.18.98,
  plan-20260714 §A.2/§A.4/§A.5)**: new `internal::upgrade::{lock,marker}` —
  `InstallDir` opens the install directory once with
  `O_DIRECTORY|O_NOFOLLOW` after §A.5 validation (absolute path, effective-
  uid ownership, no group/world write; no sticky exception granted) and
  performs every target/lock/marker/state operation fd-relative with
  `O_NOFOLLOW` (exclusive-temp + `renameat` + directory fsync atomic writes,
  refusing path separators and dot entries). The advisory `flock` upgrade
  lock uses try-lock (busy ⇒ Skip) for checks and blocking acquire for
  recovery. `.libra-official-install.json` establishes official provenance
  only when the marker parses with `install_source=official_signed_manifest`
  AND its platform/sha256/size match the actual target binary — a marker
  copied next to a foreign binary, or a binary hashing itself, never
  qualifies (§A.2). Non-Unix platforms fail closed (`UnsupportedPlatform`).
  Internal machinery only.

- **Auto-upgrade anti-rollback state and time policy (v0.18.97, plan-20260714
  §A.6/§A.7)**: new `internal::upgrade::state` — durable
  `.libra-upgrade-state.json` (atomic writes, `0600`) recording the highest
  accepted version with per-platform artifact identities, the highest control
  revision with its envelope digest, the monotone `trusted_time_floor`, the
  15-min + deterministic-jitter success cooldown and the ≤1 h failure
  backoff. Pure decision functions enforce: control-revision rollback/fork
  rejection (a pre-revocation envelope cannot replay after a revocation was
  seen), version rollback rejection with same-version artifact-identity
  immutability, required HTTPS `Date` inside the manifest lifetime, expiry
  via `effective_now = max(local, floor, Date)` (clock rollback cannot
  resurrect a manifest; a future local clock only rejects the current round
  and never poisons the floor), floor-anchored cooldown trust windows and
  cache-install refusal when the local clock sits below the floor. Corrupt
  state fails closed (skip upgrading with a warning) instead of silently
  resetting anti-rollback history. Internal machinery only.

- **Dedicated auto-upgrade HTTPS transport (v0.18.96, plan-20260714 §A.6)**:
  new `internal::upgrade::http` — a pinned reqwest client (`https_only`,
  `redirect::Policy::none()` so any 3xx is a hard failure, connect/read
  deadlines), manifest fetch bounded to 1 MiB with the HTTPS `Date` header
  captured for later time policy, effective-URL recheck before any body read,
  and artifact download streaming through a pure `SizeGate` (oversized
  `Content-Length` aborts before the body, per-chunk counting aborts past the
  manifest size, the stream must end at exactly the expected size and match
  the manifest sha256). Internal machinery only; live-server behavioral tests
  land with the `test-upgrade` integration target (§A.11).

- **Signed release-manifest verification core (v0.18.95, plan-20260714 §A.6)**:
  new `internal::upgrade::{manifest,trusted_keys,platform}` — a pure
  `verify_envelope_bytes` implementing the full §A.6 order (envelope parse
  with duplicate-key-id rejection, domain-separated Ed25519 verification via
  `ring`, strict payload semantics: `stable` channel, release-SemVer-only
  versions, exact four-platform artifact matrix with unique platforms,
  structural artifact-URL grammar pinned to
  `https://download.libra.tools/libra/releases/v{tag}/libra-{platform}` with
  `tag == version` and URL-platform == artifact-platform binding, 128 MiB
  size bound, then key-generation floor and key-validity windows). The
  compiled production trust table ships EMPTY until the release-key ceremony,
  so verification fails closed and auto-upgrade stays inert. The new
  `test-upgrade` cargo feature (plus `LIBRA_TEST=1` at runtime) is the only
  trust-root injection path; release builds contain no override code.
  Windows stays published-but-unsupported for auto-upgrade (§A.1). Internal
  machinery only — no CLI surface changes.

- **Reserved `upgrade.mode` config namespace (v0.18.94, plan-20260714 §A.3)**:
  the auto-upgrade switch now lives in `{LIBRA_HOME}/upgrade/settings.json`
  (atomic writes, `0700`/`0600` permissions on Unix), backed by a single
  Rust-side `resolve_libra_home()` that mirrors `install.sh`'s
  `LIBRA_HOME`/`HOME` rules. `libra config` routes every spelling that can
  reach `upgrade.*` through a reserved-namespace router: only single-value
  `set`/`get`/`unset` with `--global` are supported (`unset` resets to `off`
  and keeps the file; missing file reads as `off`; corrupt or unreadable files
  fail with the new `LBR-UPGRADE-001` stable code), `list --show-origin`
  renders the `file:{path}` origin, and local/system scopes, `--add`,
  `--get-all`, `--unset-all`, type conversion, section operations, conflicting
  action-flag combinations, padded spellings, and `--get-regexp` patterns
  matching `upgrade.mode` fail closed as usage errors (`LBR-CLI-002`).
  `config import` skips reserved keys with a warning, and `list` plus
  non-matching `--get-regexp` suppress stale SQLite `upgrade.*` rows. When
  `LIBRA_CONFIG_GLOBAL_DB` isolates the global config database, the upgrade
  settings follow it. The mode itself only selects the upgrade policy
  (`auto`/`manual`/`off`); the upgrade engine lands in follow-up slices.

- **Optional `lba` installer shorthand (v0.18.88)**: `install.sh` now creates
  a movable relative `lba -> libra` symlink by default. Same-version reruns
  repair a missing alias, `--no-alias` and `LIBRA_NO_ALIAS=1` opt out, and
  regular files or foreign symlinks named `lba` are preserved with a warning.
  Symlink-unavailable platforms retain a successful Libra install and receive
  an actionable warning. A deterministic full-installer smoke target covers
  clean install, repair, idempotency, opt-outs, collision safety, and fallback.
- **Reliable format-patch mail output (v0.18.86)**: adds `-1`, `--root`,
  `--minimal`, `--histogram`, `--ignore-if-in-upstream`, and diff-prefix
  controls; honors strict `format.subjectPrefix`, `format.signOff`,
  `format.outputDirectory`, and `format.suffix` defaults with CLI precedence.
  Cover-letter threading now uses unique generated message IDs, full-index is
  effective, complete series render before atomic file writes, and stdout uses
  quiet BrokenPipe handling. A seven-scenario L1 target proves plain and MIME
  Libra→Git `am`, Git→Libra `am`, config, threading, root/diff, and upstream
  patch-id behavior.

- **Minimal mail parsing plumbing (v0.18.85)**: adds repo-independent
  `libra mailinfo <msg> <patch> < mail` with Git-shaped author/email/subject/date
  metadata, body-only message output, separator-through-signature patch output,
  JSON/machine, and quiet modes. `mailinfo` and `am` now share one bounded
  UTF-8 single-part transfer/RFC 2047 parser; repository-specific patch-target
  checks remain in `am`. Both output payloads are staged before per-file atomic
  replacement, and lexical or symlink-parent aliases cannot collapse the two
  destinations. English/Chinese user docs and an eight-scenario Unix
  compatibility target cover repo-free use, folded headers, output safety, and
  fail-closed unsupported inputs.

- **Minimal mail patch sequencer (v0.18.84)**: adds `libra am <patch>...`
  with `--continue`, `--skip`, and `--abort` for bounded plain-text
  `format-patch` mail files. The implementation preserves message/author/date,
  shares the traversal- and symlink-safe text patch engine with `apply
  --check`, pins branch position across recovery, atomically advances
  branch/reflog/sequencer state, and cleans pre-stage new-file remnants on
  abort. English/Chinese user docs and a sixteen-scenario compatibility target
  cover clean-window crash resume plus rollback and document the intentionally
  deferred multipart/binary/3-way/hooks surface.

- **Previous checkout target shortcut (v0.18.83)**: adds worktree-scoped
  `libra switch -` and `libra checkout -` toggling across local branches and
  detached commits. Both commands share HEAD reflog history and record their
  own navigation actions; missing history, deleted source branches, corrupt
  records, and storage failures are rejected before HEAD, index, or worktree
  mutation. English/Chinese user and developer docs plus a nine-case
  compatibility target cover same-command, cross-command, detached, JSON, and
  fail-closed behavior.

- **Import/export fidelity (v0.18.82)**: expands `fast-export` with multiple
  revisions, incremental ranges, `--all`, annotated tags, notes, and Git path
  quoting; expands `fast-import` with inline blobs, copy/rename, annotated tags,
  note records and Git notes-tree translation, reset deletion, bounded parsing,
  object-type validation, and atomic branch/tag/note publication. `bundle`
  gains `--all`/`--branches`/`--tags`, full checksum verification, and bounded,
  hash-kind-aware `unbundle` that imports objects without moving refs. A new
  compatibility target covers Libra round trips, system-Git interoperability,
  transactional failures, repeated unbundle, and SHA-256 repositories; English
  and Chinese command/developer docs describe the supported and deferred edges.

- **Sandboxed repository hooks (v0.18.80)**: adds an Option-A-compatible
  `.libra/hooks` lifecycle for commit, checkout/switch, merge, rebase, and
  pull without executing `.git/hooks`. Hooks run with structured arguments,
  a cleared/allowlisted environment, offline required sandboxing, bounded
  input/output/file sizes, protected repository metadata, blocking pre/message
  semantics, and advisory post-hook warnings. `--no-verify`, command-specific
  pre-hook controls, and `LIBRA_NO_HOOKS` provide documented escape valves;
  English and Chinese repository-hook and command documentation are included.

- **`libra ls-files` compatibility expansion**: adds `<pathspec>...`
  filtering resolved from the caller's current working directory,
  `--error-unmatch`, and `-z` NUL-delimited text output. The release
  also extends AI/MCP read-only safety coverage for pathspec-based
  inspection and publishes the updated English/Chinese command docs.

- **`libra maintenance` command**: implements Git-compatible `maintenance`
  with subcommands `run`, `register`, `unregister`, and `status`. Supports
  tasks `gc`, `loose-objects`, `pack-refs`, `incremental-repack`,
  `commit-graph`, and `prefetch`. Includes dry-run mode, JSON output, and
  26 integration tests plus 12 unit tests.

- **Cross-cutting `--help` EXAMPLES rollout (v0.17.812..v0.17.836, sealed
  v0.17.837)**: every visible command in `src/cli.rs::Commands` now ends
  its `--help` output with an `EXAMPLES:` section listing the canonical
  invocations. Twenty-five commands grew a `pub const <CMD>_EXAMPLES`
  banner and `#[command(after_help = …)]` wiring: commit, push, merge,
  rebase, reflog, remote, mv, rm, cloud, lfs, usage, publish, grep,
  sandbox, graph, rev-parse, rev-list, symbolic-ref, db, automation,
  code, code-control, hooks, show-ref, agent. Closes
  `docs/development/commands/_general.md` cross-cutting item B.
- **`compat_help_examples_banner` regression guard (v0.17.841)**: spawns
  the libra binary, runs `<cmd> --help` for every visible command,
  and asserts the output contains an `EXAMPLES:` or `Examples:`
  section. Catches future commands that ship without an EXAMPLES
  banner.
- **`compat_command_docs_examples_section` regression guard (v0.17.851)**:
  walks every `docs/commands/<name>.md` page and asserts the body
  contains either an `## Examples` heading or a `## Common Commands`
  heading, keeping the doc surface and the runtime `--help` surface
  in sync.
- **`compat_error_codes_doc_sync` regression guard (v0.17.842)**:
  parses every `LBR-*-NNN` literal out of `src/utils/error.rs` and
  asserts each one appears in `docs/error-codes.md`. Three previously
  undocumented codes (`LBR-ADD-001`, `LBR-AGENT-001`,
  `LBR-UNSUPPORTED-001`) were added in the same patch.
- **`cli::tests::root_after_help_lists_every_visible_command`
  (v0.17.840)**: unit-level guard asserting every non-hidden command
  appears in some Command Groups row of `libra --help`. Closes the
  drift that left `fsck` and `hash-object` ungrouped.
- **`docs/commands/hooks.md` (v0.17.838)** and `docs/commands/README.md`
  Low-Level & Inspection index entry (v0.17.839): completes the
  hidden-plumbing doc coverage (every other hidden command already
  had a page).
- **Documentation Examples sections (v0.17.844..v0.17.850)**: added
  to `docs/commands/automation.md`, `docs/commands/usage.md`,
  `docs/commands/db.md`, `docs/commands/sandbox.md`,
  `docs/commands/publish.md`, `docs/commands/ls-remote.md`, and
  `docs/commands/agent.md` so every per-command doc carries an
  invocation section (enforced by
  `compat_command_docs_examples_section`).

- **`libra fsck`**: Repository integrity checker analogous to `git fsck`. Verifies
  object hash integrity (SHA1/SHA256), object format validity, ref consistency,
  index integrity, and cross-reference validation (including object type mismatch
  detection for tree entries). Supports `--verbose`, `--json`, `--objects-only`,
  `--no-cross-ref-check`, `--no-index-check`, and `--fix` (auto-repair broken refs
  and rebuild corrupted index). Exit codes use a bitmask scheme:
  bit 0 = object corruption, bit 1 = broken refs, bit 2 = index corruption.
- **`docs/commands/fsck.md`**: Comprehensive documentation for the `fsck` command
  including parameter comparison with Git, design rationale, and CI/CD examples.

### Documentation

- **Explicit non-sending `send-email` policy (v0.18.87)**: records
  `send-email` as unsupported rather than exposing a misleading transport
  stub. Libra does not read `sendemail.*`, manage SMTP credentials, or contact
  mail servers; users generate interoperable messages with `libra
  format-patch` and validate/send them with stock `git send-email` or another
  mailer. English/Chinese user guidance, the D19 governance decision, and a
  compatibility guard pin the no-network boundary.
- **AI provider env constructor policy (v0.17.1048)**: provider
  Rustdocs now define `Client::from_env()` as a source-compatible
  legacy helper for the 0.17 line and `Client::from_resolved_env(...)`
  as the preferred runtime bootstrap for repository/global
  vault-aware config. The v0.18 migration note is explicit:
  `from_env()` will be deprecated but retained for compatibility,
  while new runtime call sites should use `from_resolved_env` with a
  `LocalIdentityTarget`.
- **Root help command groups (v0.17.840)**: `fsck` and `hash-object`
  added to the `Maintenance And Plumbing` row of `libra --help`'s
  Command Groups section. Both commands were callable and documented
  but absent from the scenario-grouped index.
- **Stale src/ file-count claim refreshed (v0.17.843)**: bumped
  410 → 427 in `docs/development/commands/_general.md`'s
  `compat_all_production_unwrap_guard` description.
- **`libra code` Code-phase closeout (C1–C8)**: synced
  `docs/development/tracing/code.md`, `docs/commands/code.md`,
  `docs/commands/zh-CN/code.md`, `COMPATIBILITY.md`, and
  `tests/INDEX.md` to the shipped mode/provider/Web/MCP/session/
  approval behavior. The `run_libra_vcs` allowlist docs now list all
  ten commands (`status`, `diff`, `branch`, `log`, `show`, `show-ref`,
  `ls-files`, `add`, `commit`, `switch`) and recommend `ls-files
  --others --exclude-standard` for untracked-path inspection, matching
  the tool's own guidance.
- **Agent Gate 8 closeout docs (v0.18.21)**: re-audited the Agent
  tracing plan against the shipped code and updated
  `docs/development/tracing/agent.md` / `plan.md` to reflect the
  implemented first-batch roster, hook providers, lifecycle events,
  checkpoint/export/doctor/retention/audit behavior, and intentionally
  deferred parity items. `compat_agent_docs_contract` now also pins the
  schema/retention/raw-export wording and the current internal runtime
  source-of-truth link.
- **Mutating fix bridge deferred (no agent↔code write collaboration
  yet)**: the internal AgentRuntime serialized fix bridge is not
  enabled. `libra review --fix` and `libra investigate fix` stay
  read-only and fail closed with `LBR-AGENT-010`
  (`ERR_AGENT_FIX_BRIDGE_UNAVAILABLE`, exit 128); `libra agent`
  review/investigate produce findings only and never mutate the
  working tree through `libra code`. Because the bridge is unbuilt,
  there is no `libra agent` ↔ `libra code` mutating collaboration
  boundary to describe — findings-to-fix hand-off remains a documented
  deferral until the bridge lands with approval/sandbox/tool-ACL
  coverage.
- **External agent discovery is preview / opt-in (default off)**:
  `libra agent rpc list/trust/invoke` over external `libra-agent-*`
  binaries is disabled by default behind the `agent.external_agents.enabled`
  setting; unknown binaries are quarantined (never registered as
  callable) and built-in slug impersonation is skip-and-logged. This is
  a preview surface — enable it deliberately per repo, it is not on by
  default.
- **D1/R2 deletion propagation for agent-capture data is deferred**: a
  best-effort cloud mirror already exists via `libra cloud sync` — agent
  checkpoint blobs/trees/commits reach R2 through `object_index`, and
  `agent_session` / `agent_checkpoint` rows are mirrored to D1. Local
  erasure (`libra agent clean --gc` and session erasure) rewrites
  `refs/libra/traces` and drops the local DB / `object_index` rows, but
  it does NOT push a tombstone/delete to D1/R2, so a later
  `cloud sync` / restore from another machine could resurrect erased
  agent-capture data. Tombstone/deletion propagation to D1/R2 is
  explicitly deferred until it lands.

## [0.1.6]

### Breaking Changes

- **`libra init --separate-libra-dir` and `--separate-git-dir` removed**: non-bare repositories now always use the standard `.libra/` directory inside the worktree. Historical repositories that still use a `.libra` `gitdir:` link file are no longer detected. Migration:
  ```bash
  rm .libra
  mv /path/to/separate/storage .libra
  ```

### Changed

- **`libra init` execution/render split**: init now uses a silent execution layer internally so `clone` and other callers no longer leak init progress or JSON envelopes.
- **Human progress output**: default `libra init` now reports major phases (`Creating repository layout`, `Initializing database`, `Setting up refs`, Git conversion, vault key generation) on `stderr`.
- **Structured success output**: `libra init` now supports stable `--json` / `--machine` success envelopes with path, branch, object/ref format, repo id, vault state, Git conversion source, and SSH-key detection.
- **Git import cleanup**: `--from-git-repository` now uses the safe fetch path and suppresses nested fetch progress/JSON noise from `stderr`.
- **Vault identity alignment**: init now resolves signing identity from target-local config, global config, and commit-compatible environment fallbacks before using the built-in default identity.
- **Explicit `vault.signing=false`**: `libra init --vault false` now records the disabled signing state in `config_kv` instead of leaving it implicit.
- **Canonical config seeding**: init continues to seed only `config_kv` canonical keys (`core.*`, `libra.repoid`) and no longer relies on legacy `config` table writes.

## [0.1.5]

### Breaking Changes

- **`libra vault` subcommand removed**: Vault functionality has been integrated into `libra config`. Migration guide:
  | Old command | New command |
  |-------------|------------|
  | `libra vault generate-ssh-key` | `libra config generate-ssh-key --remote <remote-name>` |
  | `libra vault generate-gpg-key` | `libra config generate-gpg-key` |
  | `libra vault gpg-public-key` | `libra config get vault.gpg.pubkey` |
  | `libra vault ssh-public-key` | `libra config get vault.ssh.<remote-name>.pubkey` |

  Note: `<remote-name>` should be replaced with your actual remote name (usually `origin`).

- **`--system` scope removed**: System-level configuration has been removed due to multi-user permission isolation issues. Migrate existing `--system` config to `--global`:
  | Old usage | New usage |
  |-----------|----------|
  | `libra config set --system key value` | `libra config set --global key value` |
  | `libra config --get --system key` | `libra config get --global key` |
  | `libra config --list --system` | `libra config list --global` |

- **`libra config edit` not supported**: Libra uses SQLite storage; multi-value key diff-based editing cannot guarantee data consistency. Use `libra config set`/`unset`/`list` to manage configuration.

- **Config storage backend migrated**: Configuration storage moved from three-column split table (`config`) to flat key/value table (`config_kv`) with optional vault encryption. Old `Config` API is deprecated.

### Added

- **Subcommand-style CLI**: `libra config set/get/list/unset/import/path/generate-ssh-key/generate-gpg-key` with Git-compatible flag aliases (`--get`, `--list`, `-l`, `--unset`, `--add`, etc.)
- **Vault-backed encryption**: Sensitive keys (`vault.env.*`, `*.privkey`, API keys, tokens, passwords) are automatically encrypted using AES-256-GCM
- **Environment variable vault**: `vault.env.*` namespace for storing API keys and secrets with `resolve_env()` priority chain (CLI args > system env > local config > global config)
- **Per-remote SSH keys**: `libra config generate-ssh-key --remote <name>` generates isolated SSH keys per remote
- **`--encrypt` flag**: Force encryption for any config value
- **`--stdin` flag**: Read values from stdin for CI/CD pipelines
- **`--show-origin` flag**: Show which scope (local/global) each config value comes from
- **`--vault` flag**: List vault environment variables across scopes
- **`config path` subcommand**: Show config database file path
- **`config import`**: Enhanced with `--no-includes` for global scope, multi-value key handling, auto-encryption of sensitive keys
- **Sensitive key auto-detection**: `is_sensitive_key()` classifies keys by naming patterns
