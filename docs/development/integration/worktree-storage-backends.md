# Worktree storage backend architecture

Status: backend-neutral substrate implemented; ScorpioFS adapter implemented;
BrewFS SDK runtime boundary implemented

## Purpose

Libra supports worktrees whose POSIX files may come from different storage
systems. The Git model must remain identical across local directories,
repository-aware lazy projections such as ScorpioFS, and persistent distributed
volumes such as BrewFS.

The architecture separates three planes:

```text
Git plane
  Libra refs, index, objects, commits, fetch, and push

Control plane
  Libra worktree coordinator, desired state, locks, worker supervision,
  backend capability negotiation, recovery, and cleanup

Data plane
  Local directory, ScorpioFS FUSE mount, BrewFS FUSE mount, or a future
  POSIX-visible backend
```

Libra owns the first two planes. A backend driver owns only its data-plane
session.

## Core contract

`internal::worktree_backend` defines:

- `BackendKind`;
- `BackendCapabilities`;
- `BackendMountSource`;
- `BackendMountRequest`;
- `BackendMountSession`;
- `BackendHealth`;
- `BackendLifecycle`;
- `WorktreeBackendDriver`;
- `BackendRegistry`.

The driver contract contains lifecycle operations rather than a duplicate
filesystem API:

```text
mount
health
changed_paths (optional)
flush (optional)
unmount
recover
```

Build tools, editors, and ordinary Libra commands access the mounted POSIX
path. They do not call a backend SDK for individual reads and writes.

## Capability model

| Capability | Local | ScorpioFS | BrewFS |
|---|---:|---:|---:|
| POSIX worktree | yes | yes | yes |
| Revision projection | no | yes | no |
| Native changed paths | no | yes | no |
| Persistent volume | no | no | yes |
| Multi-client storage | no | no | yes |
| Flush before commit | no | no | yes |

Command code must branch on capabilities, not concrete backend names.

## Backend source types

The generic mount request distinguishes:

```text
local_directory
remote_projection
persistent_volume
```

ScorpioFS accepts `remote_projection`, including a monorepo path, base object
ID, and optional change layer.

BrewFS accepts `persistent_volume`, including a volume and optional subpath.
BrewFS does not inherently project a Mega commit. Libra must populate or import
the selected Git tree before treating a new BrewFS volume as a worktree.

## Process model

The target process model is:

```text
libra CLI
  -> Libra worktree supervisor
       -> ScorpioFS worker linked to the ScorpioFS crate
       -> BrewFS worker linked to the BrewFS crate
```

FUSE sessions outlive an individual CLI invocation. Backend workers also
isolate filesystem crashes and dependency runtimes from the Git command
process. Unix domain sockets should replace loopback HTTP as the default local
control transport; the current ScorpioFS loopback protocol remains a
compatibility transport during migration.

## Persistent layout

Libra metadata remains on a host-local filesystem:

```text
<repository-storage>/.libra/
  objects/
  refs/
  worktrees/
  backends/
    desired-state.json
    state.lock
```

Backend caches and runtime state remain separate:

```text
~/.cache/libra/backends/scorpiofs/<instance-id>/
~/.cache/libra/backends/brewfs/<instance-id>/
/run/user/<uid>/libra/
```

`.libra` must not be stored in a ScorpioFS upper layer or a BrewFS volume. A
mounted worktree contains only a reconstructable `.libra` pointer to its
host-local per-worktree gitdir.

## ScorpioFS adapter

`ScorpioFsDriver` implements `WorktreeBackendDriver` by translating generic
remote-projection requests into Antares mount requests. It exposes native
changed-path candidates and idempotent cleanup by job ID.

The existing ScorpioFS command and state files remain compatible while command
orchestration is incrementally moved onto the generic driver.

## BrewFS SDK boundary

`BrewFsDriver` accepts a `BrewFsRuntime`. The runtime is responsible for:

- constructing BrewFS metadata and object backends from named profiles;
- retaining the BrewFS SDK client and FUSE handle;
- mounting a persistent volume;
- reporting health;
- draining writes before Git commit publication;
- unmounting the session.

Configuration stores profile names, not credentials:

```toml
[backends.brewfs.team]
volume = "team-workspace"
mount_root = "/home/alice/libra-workspaces"
metadata_profile = "production-redis"
data_profile = "production-s3"
```

BrewFS 0.1.2 exports filesystem clients but keeps the complete mount assembly
used by its binary private. Libra therefore does not claim direct embedded
mount support until BrewFS exports a stable mount builder/session API. The
runtime trait is the integration seam for that API.

The minimum upstream SDK shape Libra needs is:

```rust
pub struct MountBuilder { /* metadata, object, cache, and FUSE options */ }

impl MountBuilder {
    pub async fn mount(self, mountpoint: &Path) -> Result<MountedFs>;
}

pub struct MountedFs { /* SDK client and FUSE handle */ }

impl MountedFs {
    pub fn client(&self) -> &brewfs::Client;
    pub async fn health(&self) -> Result<Health>;
    pub async fn flush(&self) -> Result<()>;
    pub async fn unmount(self) -> Result<()>;
}
```

The handle must retain all background workers and expose bounded graceful
shutdown. Configuration construction must accept credential references or
preconstructed backends so Libra never serializes secrets into `.libra`.

## Commit durability

For a backend with `flush_before_commit`, Libra must:

1. finish index updates;
2. request backend flush;
3. wait for durable completion or fail the commit;
4. construct and publish the Git commit;
5. update refs and reflogs.

This ordering prevents a commit from naming worktree content that remains only
in an unflushed client buffer.

## Migration

1. Keep existing `worktree scorpiofs attach/detach` behavior.
2. Route ScorpioFS changed-path discovery through `ScorpioFsDriver`.
3. Move attach, health, recovery, and detach orchestration to the generic
   driver.
4. Introduce a generic `worktree create --backend` command.
5. Add the BrewFS crate after its stable mount session API is available.
6. Implement a BrewFS SDK runtime and persistent-volume checkout/import flow.
7. Migrate legacy `.libra/scorpiofs/state.json` into versioned backend-neutral
   desired state.
