# `libra op` Development Notes

## Command Goal

`libra op` exposes Libra's command-level operation history. It is a Libra-native
extension rather than a Git command. The current public surface supports:

- `libra op log`
- `libra op show`
- `libra op restore`

## Compatibility

- Tier: `intentionally-different`.
- Rationale: Git has reflog and reset/restore flows, but it does not expose this
  Libra operation-graph model or the command-level restore view used here.

### Current-branch convergence contract (unreleased)

The `2026090801` branch-convergence migration is implemented in this unreleased
branch and has passed focused migration validation, including a controlled
old-binary repository upgrade. Full integration and release acceptance remain
pending. It must retain original `2026090101` (`operation_v2`)
and `2026090601` (`legacy_config_table`, #472) receipt identities and existing
timestamps, preserve legacy operation rows and `config`/`config_kv` values, and
remain forward-only.

A #472 database can already have `MAX(version) = 2026090601` while still lacking
the earlier `2026090101` receipt and v2 schema. Comparing only maxima therefore
misses this valid divergent-branch upgrade path. The higher barrier must check
the exact 0101 receipt and schema shape, target only the missing operation-v2
transition, and commit its data/schema changes and receipts atomically. An
already-applied 0101 must not be recopied; mismatched or ambiguous state must
fail closed, with failure rolling back the transaction. Do not broadly replay
every missing historical receipt: older migrations can rename/rebuild tables,
and a missing receipt is not proof that replaying that DDL is safe.

The barrier also fences binaries whose supported maximum is only 0601: after
0801 commits they must refuse the newer schema before accessing incompatible
operation tables. Keep a verified SQLite online backup and the matching old
binary before a live upgrade; linked worktrees share the database. There is no
down migration, and binary rollback or deleting receipts cannot undo this
transition. See the [user upgrade notes](../../commands/init.md#operation-v2-convergence-current-branch-unreleased).

## Implementation

- CLI entry: `src/cli.rs::Commands::Op`.
- Command implementation: `src/command/op.rs`.
- Legacy storage/service layer: `src/internal/legacy_operation.rs` and
  `src/internal/legacy_operation_model/`, using `legacy_operation*` tables.
- Legacy transaction wrapper: `src/internal/operation_wrapper.rs`.
- V2 storage/capture boundary: `src/internal/operation/{store,snapshot,middleware}.rs`.
- Versioned schema migration: `src/internal/db/migration.rs` and
  `sql/migrations/2026090101_operation_v2.sql`; the convergence barrier above is
  implemented and focused-QA validated, but unreleased. V2 tables are not
  recreated by the former v1 lazy-bootstrap helper.
- The legacy public inspection/restore path and v2 capture infrastructure coexist;
  this convergence does not remove legacy data or declare the later cutover done.

## Operation Log v2 mutation boundary

The M2/M3 implementation uses the sidecar-only working-copy pointer and does
not add a `change-id` commit header, rewrite Git commit OIDs, bump versions, or
create release artifacts. The operation middleware classifies every mutation
surface as one of `WorkspaceMutation`, `RepoMutation`, `SequencerMutation`,
`LibraStateMutation`, `ExternalOrUnknown`, `ReadOnly`, or `InternalWorker`.
Unknown classifications fail closed; read-only commands and internal workers
do not create operations. Agent shell and external VCS tools must provide
verified before/after evidence before they can be admitted.

### Request context and scope lease

`run_with_operation` binds a fresh operation-local request slot around the whole
future, including read-only and ephemeral paths. Nested operations and separate
operation futures do not share a mutable slot. Override guards restore the exact
slot they changed. Spawned tasks/threads do not inherit task locals: callers must
capture the request value and explicitly rebind it, as the status worker queues
do. Serialized standalone CLI dispatch retains its synchronous fallback; this is
not a promise that arbitrary sibling futures or every legacy entry point are
automatically isolated. See `src/internal/worktree_scope/context.rs`.

For persistent mutations, `middleware/lease.rs` acquires
`<private-gitdir>/info/operation-v2.lock` before the business callback or journal
reservation. Database opening and repository identity resolution precede it, so
a lease refusal is not a promise of zero prior database I/O. One
`File::try_lock` attempt gives a default zero contention wait, with no retry loop
or background lock waiter. Busy errors identify the repository/scope and lock
path and ask the user to wait for the other operation to finish, then retry;
other I/O errors retain their resource context.

The independently opened `File` owns the lease until close, including error or
cancellation unwinding. The persistent lock file is not a stale-lock marker:
never unlink it to resolve contention, and do not replace the private metadata
directory or lock file while writers are active. Independent linked gitdirs
have independent scope leases despite shared repository storage; other locks
may still constrain concurrency. The implementation rejects unsafe lock
leaf/parent types and rechecks the pinned parent, but relies on the existing
trusted private-gitdir boundary. It neither proves safety under arbitrary
ancestor/ABA replacement nor bounds arbitrary filesystem opens by a hard
deadline. Windows has an implementation and type-check evidence, but Windows
runtime behavior has not been validated. No CLI flags or
environment/configuration settings are added.

Lock creation uses Git-compatible shared file modes: create with `0666` subject
to umask, add `0660` for `group`, add `0664` for `all`, and apply the file bits
of an explicit numeric `core.sharedRepository` mode. Adjustment is performed
through the already-open no-follow file descriptor and also repairs an existing
owner-openable lock. Invalid or encrypted shared-mode values fail with an
actionable storage error rather than silently narrowing access.

### Isolated task replay boundary

An executor-provisioned task registry uses
`RepositoryOperationBoundary::TaskSyncBack`. Its temporary copy/FUSE workspace
is not represented as a real linked worktree with an independent HEAD/index and
operation scope, so individual mutating tool calls do not publish
`agent.tool.*` operations against main. This changes only operation ownership:
permission/hardening decisions, audit flushing, output redaction, path-alias
rebasing, and sandbox dispatch remain in the tool path. An external/unknown
mutation without hardening remains fail-closed in both boundary modes.

`ExecutionEnvironmentProvider::sync_back` resolves the main pinned scope
fail-closed and runs lease renewal, fence checks, and the complete replay inside
one main-scope `WorkspaceMutation`. A view-changing success is recorded as
`agent.task.sync-back`, with the task UUID in `causal_context_id`; a complete
unchanged view follows the normal no-op deletion path and leaves no operation.
True non-repository task workspaces retain direct replay because no repository
operation scope exists.

Wrapper failure classification depends on whether the business replay ran.
Typed lease contention becomes `ScopeLeaseBusy`. The executor first retries
only sync-back from the same completed task workspace with a bounded backoff;
these attempts do not rerun the model or consume its fresh-baseline retry
budget. Persistent contention may then fall through to the normal task retry
policy. A stale pointer or CAS failure before replay remains a
`RetryableConflict` that requires a fresh baseline. Once replay returns
success, any later snapshot, journal, view, pointer, status, or CAS publication
failure becomes `OperationPublicationAfterReplay`: it is never automatically
retried, because main may already contain the task bytes. The executor reports
that uncertainty and tells the operator to inspect `libra status` and
`libra op log`. Errors raised by the replay itself retain their original
`WorkspaceSyncError`, including partial-write context, lease classification,
and FUSE handling.

### HEAD authority correction (unreleased)

The former v2 `snapshot.rs::read_head` read a possibly absent or stale HEAD
sidecar and invented `refs/heads/main` when absent, while normal main and linked
worktrees use SQLite reference rows. A same-operation refs facet still records
ordinary in-boundary switches: do not describe all HEAD changes as silently
lost. The defect could instead hide HEAD-only external drift from content-OID
comparison and omit the detached commit from snapshot roots; actual GC data
loss has not been reproduced and also depends on other roots.

Decision reference: fixed Git `3cb9185f65410273787f74333cc027d2ea5daada`.
`worktree.c:40–55` resolves HEAD through each worktree's ref store;
`reachable.c:319–325` adds the current and other worktrees' HEAD roots separately,
including detached HEAD. `refs/files-backend.c:2743–2757` rejects a changed HEAD
symbolic type/target while explicitly noting that its check does not catch every
race. These are scoped authority/root checks, not a global atomic-snapshot
guarantee for this repair.

The implemented correction is limited to `src/internal/head.rs` and
`src/internal/operation/snapshot.rs`: an explicit database connection and pinned
worktree scope select and validate exactly one authoritative SQLite HEAD row.
Missing, duplicate or corrupt rows and query errors propagate as capture
failures, never sidecar/default-main fallbacks or successful `Full` captures.
Valid detached roots are preserved, and HEAD-only changes affect new snapshot
content identity. Existing immutable manifests are not rewritten.
Any explicit repository hash parameter on the reader is local HEAD/OID
validation, not evidence that the whole snapshot pipeline supports concurrent
mixed-hash repositories. Full integration, implementation review and release
acceptance remain pending.

### Snapshot capture

The v2 snapshot facets capture the raw index byte-for-byte together with
sequencer and sparse-view state. `WorkspaceSnapshotV2` excludes ignored files
by default and records bounded scans as partial rather than presenting an
incomplete snapshot as complete. The focused contracts are registered in
`tests/INDEX.md`; the full restore engine remains OL-10.

The current-branch capture hardening is implemented but remains unreleased:
for a stable worktree, tracked `160000` gitlinks and descendants must not be
enumerated, read, or written as parent snapshot blobs. Both lexical paths and physical directory aliases,
including case/Unicode aliases recognized by the filesystem, must respect that
opaque boundary. A nonliteral file/symlink path with the same filesystem identity
can represent a hardlink alias; when safety cannot be established, the required
fallback is to omit unsafe capture and report `Partial`, not assume exclusion
was complete. Unknown identity or corrupt/unreadable index evidence likewise
requires conservative `Partial`, not an empty-index unrestricted scan. This
does not specify that all hardlinks cause `Partial`.

Snapshot ignore checks use `utils/ignore/bounded.rs::BoundedIgnoreWalk` with the
original scan deadline, captured exclusion layers, no-follow dirent type and a
walk-local error epoch. Config discovery is prewarmed on the caller before the
bounded worker lookup. Unknown ignore verdicts, ignore-file read failures
(including invalid UTF-8) or deadline expiry discard the visible-file listing
and yield `Partial`; `list_visible_files` returns no candidates to hash or persist
from that listing. This does not mean that every directory-read error discards
the entire listing. An error must not become an empty permissive matcher on a
later walk, and a late worker must not poison another walk's error state. The
shared raw-source cache in `utils/util.rs` holds its mutex only for lookup/insert,
not file I/O or parsing; failed reads are not cached as successful empty rules.
Invalid glob patterns retain their existing warning/skip behavior.

Detected identity drift likewise invalidates the listing and produces
`Partial`. These sequential checks do not freeze the external filesystem or
guarantee an atomic view under arbitrary continuous concurrent changes,
including ABA between checks. Listing may read ignore rules, so it is not
metadata-only. The original scan deadline does not guarantee a 30-second hard
limit spanning all capture phases, config prewarming or arbitrary filesystem I/O.

Command outcome, snapshot completeness, and restore capability are separate.
Failure may still leave operation/pre-snapshot records, but never authorizes
opaque nested-content capture. `Full` describes capture completeness within its
supported scope; neither it nor `--force` implements full restore or adds support
for uncaptured state. See the [user capture contract](../../commands/op.md#current-branch-capture-contract-unreleased).

## Current Behavior

- `op log` lists operations by repository with pagination and exact command
  filtering.
- `op show` resolves an operation id or `@{n}` reference and can print the
  captured view snapshot.
- `op restore` restores HEAD and captured branch refs from a previous operation
  view and records a new successful restore operation. It also **prunes** local
  branches that are absent from the target view, so restore reproduces that
  operation's exact local-branch set rather than only updating named refs. Never
  pruned: the restored HEAD branch, remote-tracking refs, the locked branches
  (`main`/`intent`/`traces`/`agent-traces`), and the reserved `libra/` namespace
  (AI history `libra/intent`, orchestrator `libra/src`/`libra/target`).
  `--dry-run` previews the prune (and the restore) without writing.

## Remaining Gaps

- Broader command coverage and the full v2 restore engine remain incremental;
  the legacy public restore contract must not be confused with v2 capture.
- Final integrated validation of request context, ignore capture and scope
  leases, platform runtime coverage, and whole-tree integration/release gates
  remain open; these notes do not establish validation or release acceptance.
