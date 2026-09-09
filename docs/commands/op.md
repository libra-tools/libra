# `libra op`

Inspect and restore command-level operation history.

## Synopsis

```bash
libra op log [OPTIONS]
libra op show [OPTIONS] <OP_REF>
libra op restore [OPTIONS] <OP_REF>
```

## Description

`libra op` provides a command-line surface over the operation graph persisted by
the operation service and wrapper layers.

It currently supports three subcommands:

- `op log`: list recorded operations with pagination and optional command filter.
- `op show`: inspect one operation and, optionally, the captured restore view.
- `op restore`: move HEAD and branch refs back to a previously captured view.

## Operation References

`<OP_REF>` may be either:

- A concrete operation id, for example `019e3f00-8ee5-7e62-a54c-0ab1f1bba0f9`
- A reflog-style index, for example `@{0}` for the newest operation or `@{1}`
  for the previous one

## `libra op log`

List operation history.

```bash
libra op log [--page <N>] [-n <PER_PAGE>] [--command <NAME>] [--verbose]
```

### Options

### `-n, --number <PER_PAGE>`

Number of operations to show per page. Defaults to `50`.

```bash
libra op log -n 20
```

### `--page <N>`

Page number to display. Defaults to `1`.

```bash
libra op log --page 2 -n 20
```

### `--command <NAME>`

Filter operations by exact command name, such as `branch` or `op restore`.

```bash
libra op log --command branch
libra op log --command "op restore"
```

### `--verbose`

Show one operation as a multi-line block with actor, status, and timestamp.

```bash
libra op log -n 5 --verbose
```

## `libra op show`

Inspect a single operation.

```bash
libra op show [--view] <OP_REF>
```

### Options

### `--view`

Print the captured restore view, including HEAD target and refs.

```bash
libra op show @{0} --view
```

## `libra op restore`

Restore the supported HEAD/ref state from a previously captured operation view,
not arbitrary working-tree or nested-repository contents. HEAD and the
captured branch refs are reset to the target view, and local branches that are
absent from that view are pruned, so the restore reproduces the operation's
exact local-branch set. The restored HEAD branch is always kept; remote-tracking
refs and Libra-owned internal refs (the locked `main`/`intent`/`traces`
branches and the reserved `libra/` namespace, e.g. the AI history branch
`libra/intent`) are never pruned.

```bash
libra op restore [--force] [--dry-run] <OP_REF>
```

### Options

### `--force`

Allow restore to proceed even if the working tree is dirty. This does not add
restore capabilities or turn a `Partial` capture into a complete snapshot.

```bash
libra op restore @{0} --force
```

### `--dry-run`

Show the target HEAD and refs without writing a new restore operation.

```bash
libra op restore @{0} --dry-run
```

## Examples

```bash
# List the newest ten operations
libra op log -n 10

# Show only branch operations on page 2
libra op log --command branch --page 2 -n 5

# Inspect the latest operation and its view snapshot
libra op show @{0} --view

# Restore to the previous operation view
libra op restore @{1}

# Preview a restore without changing repository state
libra op restore @{1} --dry-run
```

## Notes

- `op restore` records a new `op restore` operation on success.
- `op restore --dry-run` does not write a new operation.
- Restore resets HEAD and the branch refs captured in the target view, and
  prunes local branches that are absent from that view (the restored HEAD branch
  is always kept; remote-tracking refs are left untouched).

### Operation-scoped execution (unreleased)

For mutations routed through the v2 operation middleware, each operation keeps
its own pinned repository/worktree request context, including across asynchronous
work. Independent linked worktrees use their own private gitdirs and scope
leases even when they share repository storage; this lease does not serialize
them with each other. Other repository locks and command restrictions still apply.

The scope lease at `<private-gitdir>/info/operation-v2.lock` makes one
non-waiting acquisition attempt: the default contention wait is zero, with no
retry loop. A competing operation in the same scope is refused before its
business callback or operation journal starts. A busy error identifies the
scope and lock path; wait for the other operation to finish, then retry.

The lock is owned by an open file and released when that file closes. The lock
file itself may remain after the operation. **Do not delete it to resolve
contention**, or replace the private metadata directory while operations are
active. These rules do not promise a hard deadline for arbitrary filesystem
I/O or safety under arbitrary concurrent ancestor replacement. They add no
CLI flags or environment/configuration settings.

The persistent lock follows the repository's existing
`core.sharedRepository` file permissions. In a group/all/numeric shared
repository, a lock first created by one user remains openable by the users that
the shared mode permits; the default/false/umask modes continue to follow the
process umask.

### Isolated agent task sync-back operations (unreleased)

An isolated `libra code` DAG task runs its tools in a temporary copy or FUSE
workspace, not a true linked worktree with its own operation scope. Mutating
tool calls there still pass permission, hardening, audit, redaction, and
sandbox checks, but they do not each publish an `agent.tool.*` operation
against the main workspace.

When the task finishes, Libra serializes replay into the main workspace. A
successful replay that changes the captured view publishes one
`agent.task.sync-back` `WorkspaceMutation`; its `causal_context_id` stores the
task UUID so the operation can be attributed to the task. A replay that leaves
the view unchanged does not create an operation.

If the main scope lease is busy, the scheduler first retries only sync-back
with a short bounded backoff, preserving the completed task workspace and not
consuming the task's fresh-baseline retry budget. Persistent contention may
then use the normal task retry policy. An operation pointer/CAS change before
replay instead requires a fresh baseline. If replay has completed but
post-snapshot or operation publication fails, Libra does **not** retry
automatically: the main workspace may already contain the task changes. Follow
the error guidance and inspect `libra status` plus `libra op log` before
deciding whether to recover or rerun.

### HEAD capture authority (unreleased)

New v2 captures read the pinned worktree scope's SQLite HEAD row, not a HEAD
sidecar file or a fabricated `main` fallback. Missing, duplicate or corrupt
HEAD rows, and query failures, reject capture rather than report a successful
snapshot with an assumed HEAD. A valid detached HEAD commit remains a referenced
snapshot root, and HEAD-only changes affect the new snapshot's content identity.
This behavior is implemented on the unreleased branch; full integration,
implementation review and release acceptance remain pending.

This correction does not rewrite existing immutable manifests or establish
that actual garbage-collection data loss has occurred. It adds neither full
restore support nor concurrent mixed-hash snapshot support.

### Current-branch capture contract (unreleased)

The operation-v2 behavior below is implemented on this unreleased branch;
integration and release acceptance remain pending. The existing
legacy `op` inspection/HEAD-ref restore surface and v2 capture infrastructure
coexist. A v2 `Full` capture is not a promise of full restore support.

- For a stable worktree, a tracked gitlink (index mode `160000`) and its
  descendants are opaque to the parent snapshot: nested content must not be
  enumerated, read, or persisted as parent-repository snapshot blobs. This is
  not a submodule backup feature.
- The boundary must hold for lexical paths and physical directory aliases,
  including case/Unicode spelling aliases on filesystems that identify them as
  the same directory. A different spelling must not bypass the boundary.
- A nonliteral file or symlink path with the same filesystem identity can also
  represent a hardlink alias. When the scanner cannot safely establish the
  boundary, it must omit unsafe capture and mark the snapshot `Partial`, rather
  than assume the path is safe. This is not a claim that every hardlink causes
  `Partial`.
- A corrupt/unreadable index or unknown filesystem identity must likewise lead
  to conservative `Partial` capture, never an unrestricted scan based on an
  assumed empty index. Missing content must not be reported as fully captured.
- Ignore checks use the scan's original deadline, not a fresh budget for each
  path. An unknown ignore decision, a failed ignore-file read (including invalid
  UTF-8), or expiry of that deadline makes capture `Partial` and discards the
  visible-file listing; files from that rejected listing are not hashed or
  persisted. An unreadable rule set is not treated as an empty, permissive one.
- When identity drift is detected, the listing is likewise discarded and capture
  marked `Partial`. Snapshotting does not freeze the external filesystem or
  guarantee an atomic view under arbitrary continuous concurrent changes,
  including ABA changes away and back between checks. Listing may read ignore
  rules; it is not metadata-only. The scan deadline is not a 30-second hard
  deadline for all capture phases or arbitrary filesystem I/O.
- Command outcome, snapshot completeness, and restorability are separate. A
  failed command can still leave operation/pre-snapshot records; this does not
  permit capturing opaque nested content. Neither `Full` nor `--force` grants
  restore support for uncaptured contents or unsupported state.

The unreleased database transition is documented under
[operation-v2 convergence](init.md#operation-v2-convergence-current-branch-unreleased).
