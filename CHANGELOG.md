# Changelog

## [Unreleased]

### Changed

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
  detected rename as Git's single `2 R<score> N... <mH> <mI> <mW> <hH> <hI>
  R<pct> <new>\t<old>` record — with the real HEAD tree modes/hashes, index
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
