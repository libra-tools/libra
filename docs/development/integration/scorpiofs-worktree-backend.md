# ScorpioFS remote worktree backend

Status: MVP implemented and validated against a real Mega-backed FUSE mount

This document describes the ScorpioFS-specific adapter. The backend-neutral
contracts, BrewFS extension point, storage ownership, and process model are
defined in
[`worktree-storage-backends.md`](worktree-storage-backends.md).

## Libra-owned state and direct crate execution

Libra is the authoritative owner of ScorpioFS desired state. It persists the
worker identity and every requested mount under
`.libra/scorpiofs/state.json`, including lifecycle transitions through
`mounting`, `ready`, `unmounting`, and `recoverable_error`.

On Linux, `worktree scorpiofs attach` starts or reuses a hidden
`libra scorpiofs-worker` process by default. That worker links the `scorpiofs`
crate directly and keeps FUSE sessions alive after the invoking CLI process
exits. ScorpioFS runs with external state ownership, so its in-memory mount
registry is an execution cache only: it must not persist, recover, or decide
the desired mount set.

The HTTP transport remains available only as an explicit compatibility mode:

```text
libra worktree scorpiofs attach \
  --endpoint http://127.0.0.1:2725/antares \
  --remote-path /project/aardvark-dns \
  --job-id aardvark-dns
```

Without `--endpoint`, Libra starts the crate-backed worker using
`--config-path scorpio.toml`. When the final Libra-owned mount is detached,
Libra asks its worker to shut down gracefully.

## Summary

Libra integrates ScorpioFS as a remote-projection worktree backend. Libra
remains the only owner of version-control semantics and desired lifecycle
state. ScorpioFS owns remote file materialization, FUSE execution, changelist
layers, writable upper layers, and live mount sessions.

The integration must not:

- reimplement Git object, index, ref, merge, or transport logic in ScorpioFS;
- link the complete Libra application into ScorpioFS;
- mount Libra's FUSE worktree on top of a ScorpioFS FUSE mount;
- store the persistent Libra repository database inside an ephemeral Antares
  upper layer;
- expose arbitrary Libra command execution through ScorpioFS's unauthenticated
  HTTP API.

## Ownership

### Libra owns

- repository identity;
- common object storage;
- the index;
- HEAD, branches, refs, and reflogs;
- commit and tree construction;
- status, diff, restore, checkout, merge, rebase, and stash semantics;
- remotes, credentials, fetch, pull, and push;
- hooks and signing;
- Mega single-commit push preflight.

### ScorpioFS owns

- the Mega-backed read-only base filesystem;
- lazy tree and blob materialization;
- optional changelist layers;
- per-mount writable upper layers;
- FUSE inode and file-handle lifecycle;
- mount creation, readiness, recovery, and deletion;
- efficient reporting of paths changed in the writable view;
- switching a mount to a different immutable base snapshot.

### The integration layer owns

- mapping a Libra linked worktree to a ScorpioFS mount;
- the local protocol and capability negotiation;
- lifecycle and operation locks;
- recreating the `.libra` worktree pointer after remount;
- coordinating base-snapshot changes for pull, switch, and reset;
- converting service errors into stable Libra errors.

## Why the integration belongs in Libra

Libra already exposes a library entry point and implements the full VCS command
surface. It also has linked-worktree scoping for local HEAD, index, FETCH_HEAD,
sequencer, rebase, and advisory state.

ScorpioFS already exposes an Antares control plane and an isolated userspace
overlay. Making ScorpioFS a Libra worktree backend therefore adds one adapter
instead of duplicating VCS behavior.

Libra's optional `worktree-fuse` feature is not used for this backend. That
feature creates a local overlay from a local lower directory. A ScorpioFS
backend is already a mounted remote overlay; nesting the two introduces
duplicate mount ownership, cleanup ambiguity, and unnecessary filesystem
overhead.

## Storage layout

Persistent Libra state lives outside the ScorpioFS mount:

```text
<main-repository>/.libra/
├── libra.db
├── objects/
├── refs/
└── worktrees/
    └── scorpiofs/
        └── <workspace-id>/
            ├── commondir
            ├── worktree_id
            ├── index
            ├── HEAD
            ├── FETCH_HEAD
            └── backend.json
```

The mounted worktree contains only a reconstructable `.libra` gitdir pointer:

```text
<scorpiofs-mount>/.libra
```

The pointer resolves to the persistent worktree gitdir. It may be recreated
after every mount without changing repository history or worktree identity.

`backend.json` contains no credentials:

```json
{
  "schema_version": 1,
  "backend": "scorpiofs",
  "endpoint": "unix:///run/scorpiofs/control.sock",
  "mount_id": "1a78c97f-68b7-4873-bfe2-2d67f3768b23",
  "job_id": "build-123",
  "remote_path": "/project/aardvark-dns",
  "base_oid": "0123456789abcdef",
  "cl": "1XFJ4PGK"
}
```

The canonical identity is a stable Libra `worktree_id`, not the transient
ScorpioFS `mount_id`.

## User-facing command model

The backend is managed through Libra:

```text
libra worktree scorpiofs attach \
  --endpoint http://127.0.0.1:2725/antares \
  --remote-path /project/aardvark-dns \
  --job-id dev-aardvark

libra worktree scorpiofs detach <mountpoint>

libra worktree list
libra worktree repair
libra worktree remove <path>
```

After the worktree is ready, ordinary Libra commands run inside it:

```text
libra status
libra add .
libra commit -m "..."
libra fetch origin
libra push --dry-run origin main:main
```

Backend-specific options belong to `worktree scorpiofs attach`; normal VCS
commands must not grow ScorpioFS-specific flags.

An attached ScorpioFS worktree uses a private detached HEAD. Pushes from it
must therefore name both the remote and an explicit source/destination
refspec, such as `main:main`. A default push that needs Libra to infer the
current branch remains rejected. The `--dry-run` form validates Mega discovery
and the update plan without mutating the remote.

The existing local-copy and optional local-FUSE worktree backends remain
compatible. A serialized worktree record gains a backward-compatible backend
descriptor whose default is `local`.

## Control protocol

### Transport

Production integration uses a local Unix domain socket. A loopback HTTP endpoint
may be supported for development, but must require an explicit opt-in and must
not accept credentials or arbitrary commands.

The first implementation may use the existing Antares loopback HTTP API behind
the backend client. The public Rust interface must hide the transport so it can
move to the Unix socket without changing command code.

### Version negotiation

Every client begins with:

```json
{
  "protocol_version": 1,
  "client": "libra",
  "client_version": "0.19.40"
}
```

The service responds with:

```json
{
  "protocol_version": 1,
  "service": "scorpiofs",
  "capabilities": [
    "mount.v1",
    "ready.v1",
    "changes.v1",
    "base-snapshot.v1"
  ]
}
```

Libra must fail closed when a required capability is unavailable. Optional
capabilities may select a documented slower fallback.

### Mount request

```json
{
  "job_id": "dev-aardvark",
  "path": "/project/aardvark-dns",
  "cl": null,
  "base_oid": "0123456789abcdef"
}
```

Response:

```json
{
  "mount_id": "1a78c97f-68b7-4873-bfe2-2d67f3768b23",
  "mountpoint": "/var/lib/scorpiofs/antares/mnt/1a78c97f",
  "base_oid": "0123456789abcdef",
  "ready": false
}
```

Create-by-`job_id` remains idempotent.

### Changed-path request

Libra must not recursively scan the full remote monorepo for `status` or
`add .`. ScorpioFS reports candidate paths from the CL and writable upper
layers:

```json
{
  "mount_id": "1a78c97f-68b7-4873-bfe2-2d67f3768b23",
  "generation": 42,
  "changes": [
    { "kind": "modified", "path": "src/lib.rs" },
    { "kind": "added", "path": "notes.txt" },
    { "kind": "deleted", "path": "src/old.rs" },
    {
      "kind": "renamed",
      "path": "src/new.rs",
      "source_path": "src/previous.rs"
    }
  ]
}
```

This is a candidate set, not authoritative Git status. Libra still applies
ignore rules, pathspecs, index comparison, content hashing, rename policy, and
Git-compatible output.

If `changes.v1` is unavailable, Libra may scan only the physical writable
layer. It must warn before falling back to a full mounted-tree walk.

## Worktree creation transaction

`libra worktree scorpiofs attach` performs:

1. Validate the current Libra repository and requested remote path.
2. Negotiate backend capabilities.
3. Reserve a stable Libra worktree ID.
4. Create the persistent per-worktree gitdir and `backend.json`.
5. Request or recover the idempotent ScorpioFS mount.
6. Wait for mount readiness with a bounded timeout.
7. Attach the worktree gitdir pointer inside the mount.
8. Seed the worktree index from the selected Libra commit without populating
   files.
9. Register the worktree in Libra's common worktree state.
10. Mark the backend record ready.

Failures roll back in reverse order. A mount that cannot be deleted is recorded
as orphaned and reported with a repair command; it must not be silently
forgotten.

## Status and detach consistency

ScorpioFS is the authority for the writable-view candidate set. Libra does not
walk the full mounted monorepo during `status`, `add`, or the dirty-worktree
check that precedes `detach`. It asks `changes.v1` for candidate paths and then
applies normal Libra index, ignore, hashing, and rename rules to only those
paths.

This rule is also required for correct cleanup. A FUSE mount can contain
implementation-local upper-layer artifacts that are not part of the
ScorpioFS-reported writable view. A raw recursive disk scan could therefore
make an already committed worktree impossible to detach. Detach uses the same
candidate-path collection as `status`; it still refuses to detach when the
service reports staged or unstaged Libra-visible changes. The persistent
`.libra` pointer is metadata, not a user file or staged change.

## Normal command behavior

### Status

1. Resolve the current linked-worktree scope.
2. Load and validate `backend.json`.
3. Ask ScorpioFS for changed-path candidates.
4. Let Libra compare HEAD, index, and mounted file content.
5. Enumerate untracked files from the writable layer, not the remote base.

### Add

Libra applies pathspec and ignore semantics, reads selected mounted files,
writes blobs, and updates the worktree-local index. Deleted candidates stage as
deletions. ScorpioFS does not create Git objects.

### Commit

Libra builds trees and commits from the index, updates refs and reflogs, runs
hooks, and signs when configured. ScorpioFS is not involved.

### Fetch

Libra updates objects and refs. Fetch does not change the mounted base or
writable filesystem.

### Push

Libra performs transport, authentication, pack construction, and ref updates.
Mega-specific single-commit policy is checked in Libra before transport.
ScorpioFS does not receive credentials or push data.

The initial validation uses an explicit refspec and `--dry-run`:

```text
libra push --dry-run origin main:main
```

This proves remote discovery, Smart HTTP planning, detached-worktree refspec
handling, and Mega update preparation without changing the remote. A real
push remains an explicit user action and is subject to Libra's Mega
single-commit preflight.

## Branch and base-snapshot changes

There are two implementation stages.

### Stage A: upper-layer delta

Checkout, switch, restore, and reset write the difference between the immutable
base tree and target tree into the writable layer. Deletions use the overlay's
supported whiteout representation.

This provides correctness first but may grow the upper layer after repeated
branch switches.

### Stage B: transactional base switch

For clean worktrees, Libra requests a new immutable base snapshot:

1. Acquire the exclusive VCS and mount lifecycle leases.
2. Verify or stash local changes.
3. Prepare a replacement mount for the target commit.
4. Attach the existing persistent Libra worktree gitdir.
5. Verify the new view and index.
6. Atomically publish the replacement mount.
7. Delete the old mount.

If publication fails, the old mount remains active. If old-mount cleanup fails,
the operation succeeds with an explicit orphan warning and repair record.

`pull`, branch `switch`, `checkout`, `reset --hard`, and `rebase` must not update
Libra refs while leaving the user on an unrelated old base view.

## Locks and lifecycle

Each backend worktree has:

- a shared read lease for status, diff, log, and read-only inspection;
- an exclusive VCS lease for add, commit, checkout, merge, rebase, and reset;
- an exclusive lifecycle lease for mount, base switch, repair, and unmount.

Unmount refuses to race with a VCS operation. Shutdown stops admitting new
operations, waits for bounded graceful completion, persists recovery state, and
then unmounts.

The backend state machine is:

```text
Detached
  -> Mounting
  -> Ready
  -> SwitchingBase
  -> Ready
  -> Unmounting
  -> Detached

Any state may enter RecoverableError.
```

## Error contract

Backend failures map to stable Libra error categories:

- backend unavailable;
- protocol incompatible;
- mount rejected;
- mount readiness timeout;
- stale mount identity;
- changed-path generation lost;
- base snapshot unavailable;
- worktree busy;
- cleanup incomplete;
- backend state corrupt.

Messages include the operation, worktree path, job ID, and recovery action. They
must not include tokens, credential-bearing URLs, signing material, or file
contents.

## Security

- Prefer a Unix socket owned by the current user or service group.
- Validate that every returned mountpoint is inside the configured ScorpioFS
  mount root.
- Validate that every worktree gitdir is inside Libra common storage.
- Never pass credentials on a process command line.
- Do not expose a generic "run Libra command" ScorpioFS endpoint.
- Treat remote paths, CL names, mount IDs, and changed paths as untrusted.
- Reject absolute changed paths and paths containing parent traversal.
- Preserve the unauthenticated Antares API warning until a protected transport
  is available.

## Compatibility

- Existing Libra repositories and local worktrees default to backend `local`.
- Existing serialized worktree records load without migration.
- `worktree-fuse` remains optional and independent.
- Native Git fallback remains available for explicitly unsupported Libra
  behavior.
- Hooks remain under Libra metadata; no synthetic `.git/hooks` directory is
  created.
- Advanced commands that are not safe in linked worktrees remain guarded until
  their state is worktree-scoped.

## Implementation phases

### Phase 1: backend substrate

- Add versioned backend types and persistent backend records.
- Add a transport-independent ScorpioFS client.
- Add backend-aware worktree registration, list, repair, and removal.
- Use the existing Antares HTTP API for mount, readiness, and delete.

### Phase 2: core VCS workflow

- Run status, add, commit, fetch, and push in the attached linked worktree.
- Add Mega single-commit push preflight.
- Add lifecycle locking and recovery tests.

### Phase 3: changed paths

- Add `changes.v1` to ScorpioFS.
- Consume candidates in Libra status and add.
- Add generation, overflow, rename, deletion, and ignore tests.

### Phase 4: mutable worktree operations

- Validate restore, path checkout, switch, reset, merge, and stash on the
  writable overlay.
- Add whiteout and metadata-operation coverage.

### Phase 5: immutable base snapshots

- Add commit-addressed bases and transactional base switching to ScorpioFS.
- Integrate pull, branch switching, reset, and rebase.
- Add crash recovery and orphan cleanup.

### Phase 6: production validation

- **Validated** mount/open, status, add, commit, fetch, explicit push planning,
  and detach against `project/aardvark-dns` on Mega. The test ran in an
  isolated Linux user and mount namespace and verified that
  `.libra/scorpiofs/state.json` has an empty `mounts` map after detach.
- **Implemented test coverage** includes endpoint validation, state locking,
  lifecycle transitions, changed-path validation, attach idempotency, and the
  detach regression where an unreported local artifact must not block a clean
  ScorpioFS worktree.
- Validate Buck2 builds on the same mount.
- Test restart recovery, concurrent worktrees, cancellation, and cleanup.
- Document native Git fallback and operational diagnostics.

## Verified command trace

The end-to-end test executes this sequence against the deployed Mega
subrepository:

```text
libra clone https://git.rk8s.xuanwu.openatom.cn/project/aardvark-dns control
libra worktree scorpiofs attach --config-path scorpio.toml \
  --remote-path /project/aardvark-dns --job-id <unique-job>
cd <mountpoint>
libra status --porcelain
libra fetch origin
libra add libra-scorpiofs-e2e.txt
libra commit -m "test: validate Libra ScorpioFS backend"
libra push --dry-run origin main:main
libra worktree scorpiofs detach <mountpoint>
```

The test creates only a temporary local commit in the isolated worktree. It
does not publish a remote commit or alter the deployed Mega branch.

## Deliberate current limits

- The compatibility HTTP control transport is still supported; a protected
  Unix-socket transport is the production target.
- Libra uses ScorpioFS as a POSIX data plane and does not duplicate Git logic
  in the filesystem service.
- Automatic transactional base switching, restart recovery, and concurrent
  mount stress are not yet validated end-to-end.
- Buck2-on-ScorpioFS validation remains separate work; passing VCS lifecycle
  tests is not a Buck2 compatibility guarantee.

## Acceptance criteria

The first production-capable milestone is complete when:

- a ScorpioFS mount attaches as a persistent Libra linked worktree;
- normal Libra `status`, `add`, `commit`, `fetch`, and `push` work inside it;
- unmount/remount preserves Libra metadata and worktree identity;
- status and `add .` do not walk the full remote monorepo;
- no credentials pass through ScorpioFS;
- mount and VCS operations cannot race destructively;
- failures leave a diagnosable and repairable state;
- focused unit tests and a real Mega/FUSE end-to-end test pass.
