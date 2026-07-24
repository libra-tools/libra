# `libra worktree`

Manage multiple working trees attached to this repository.

**Alias:** `wt`

## Synopsis

```
libra worktree add <path>
libra worktree list
libra worktree lock <path> [--reason <text>]
libra worktree unlock <path>
libra worktree move <src> <dest>
libra worktree prune
libra worktree remove <path>
libra worktree umount <path> [--cleanup]
libra worktree repair [<path>]
libra worktree repair --migrate-layout [--dry-run] [<path>]
```

## Description

`libra worktree` manages multiple working trees that share a single repository database and object store. This allows you to have several checkouts of the same repository simultaneously, which is useful for working on multiple branches at once, running builds while editing code, or testing changes in isolation.

Each linked worktree is a directory containing its own real `.libra` gitdir — a local directory (not a symlink) that holds the worktree's private `HEAD`, index, and `HEAD` reflog, plus a `commondir` pointer to the shared storage and a stable `worktree_id`. The main worktree is the original repository directory. All worktrees share the same SQLite database, object store, branch/tag/remote refs, and configuration, but each keeps its own checked-out branch and staging state. (A worktree created by an older Libra version may still use the legacy shared-`.libra` symlink layout; run `libra worktree repair` to check.) The registry file `worktrees.json` is versioned (`schema_version: 2` since v0.19.57): each linked entry persists its stable `worktree_id`, a legacy v1 file is upgraded in place by the first mutating worktree command (ids backfilled from each worktree's gitdir; lockless readers like `worktree list` read a v1 file without rewriting it), and older binaries are refused at the database layer before they can misread or rewrite the v2 file.

Worktree metadata is persisted in a `worktrees.json` file inside the `.libra` storage directory. Each entry tracks the filesystem path, whether it is the main worktree, its lock status, and an optional lock reason. The state file is written atomically via a temporary file rename to prevent corruption.

When a new worktree is added and HEAD points to a commit, the worktree is automatically populated with the committed content from HEAD (not staged index changes).

## Options

### Subcommand: `add`

Create a new linked worktree at the given filesystem path.

| Argument / Flag | Description |
|-----------------|-------------|
| `<path>` | Filesystem path for the new worktree. Can be relative or absolute. The directory is created if it does not exist. Must not be inside `.libra` storage, must not already be registered, and must be empty if it exists. |
| `<branch-or-commit>` | Optional target. An existing branch is checked out ATTACHED (refused before any side effect if any worktree — including the invoking one — already has it out). Anything else must resolve as a commit-ish and seeds a DETACHED worktree populated from that commit. A nonexistent branch fails closed: Git's remote-branch DWIM, `worktree.guessRemote`, and `--track`/`--no-track` are deferred. |
| `--detach` | Detach HEAD even when the target names a branch (the branch stays free for checkout elsewhere). |
| `-b, --create-branch <NEW_BRANCH>` | Create `NEW_BRANCH` at `<branch-or-commit>` (default: the source worktree's HEAD) and check it out. Refused if the branch already exists (`-B`/`--force` are deferred); any later failure rolls the branch back — no branch-only residue. |

Without a target the new worktree is created **detached at the source
commit** — intentionally different from Git's default (which creates a
branch named after the path basename). `--lock`, `--orphan`, and
`--no-checkout` are deferred; use the separate `worktree lock` subcommand.

```bash
# Detached at the source commit (Libra's default)
libra worktree add ../my-feature
libra --json worktree add ../my-feature

# Check an existing branch out
libra worktree add ../fix-1 hotfix

# Detached at a commit-ish (tag, sha, branch tip)
libra worktree add --detach ../probe v1.2.0

# Create a new branch from a start point and check it out
libra worktree add -b topic ../topic main
```

### Subcommand: `list`

List all registered worktrees and their state. `--porcelain` emits a stable,
machine-readable format: for each worktree a `worktree <path>` line, that
worktree's own `HEAD <sha>` line and either a `branch <ref>` or a `detached`
line (each worktree owns its HEAD), and a `locked [<reason>]` line when locked,
with a blank line between worktrees. A worktree whose HEAD cannot be resolved
(a legacy shared-`.libra` layout, or a missing/corrupt scope) omits the HEAD
lines rather than being mislabeled with another worktree's commit.

```bash
libra worktree list
libra worktree list --porcelain
libra --json worktree list
libra --machine worktree list
```

Structured output uses the `worktree.list` command envelope. Each entry reports
`kind`, `path`, `is_main`, `locked`, `lock_reason`, whether the path currently
exists on disk, the persisted `worktree_id`, and the lifecycle `state`
(`active`, `detached_from_registry`, or `tombstone` — see `remove`), and the on-disk `layout` (`main`, `linked-v2`, `legacy-symlink`, `missing`, `corrupt`; porcelain adds a matching `layout` line per entry). In a `legacy-symlink` worktree (pre-isolation shared `.libra`), read-only commands keep working but state-mutating commands refuse with `LBR-REPO-003` — run `libra worktree repair --migrate-layout <path>` from the main worktree. Target-oriented lifecycle commands (`worktree remove <path>` in both modes and `worktree repair <path>`) also refuse a legacy-symlink target — the shared symlink would route their writes into MAIN storage — until the migration completes.

### Subcommand: `lock`

Mark a worktree as locked to prevent it from being pruned or removed.

| Argument / Flag | Description |
|-----------------|-------------|
| `<path>` | Filesystem path of the worktree to lock. |
| `--reason` | Optional human-readable explanation for why the worktree is locked. |

```bash
# Lock a worktree
libra worktree lock ../my-feature

# Lock with a reason
libra worktree lock ../my-feature --reason "long-running experiment"
libra --json worktree lock ../my-feature --reason "long-running experiment"
```

### Subcommand: `unlock`

Remove the lock from a previously locked worktree. Idempotent: unlocking an already-unlocked worktree is a no-op.

| Argument | Description |
|----------|-------------|
| `<path>` | Filesystem path of the worktree to unlock. |

```bash
libra worktree unlock ../my-feature
libra --machine worktree unlock ../my-feature
```

### Subcommand: `move`

Move or rename an existing linked worktree. The directory is renamed on disk and the registry is updated. Cannot move the main worktree or a locked worktree.

| Argument | Description |
|----------|-------------|
| `<src>` | Current filesystem path of the worktree. |
| `<dest>` | New filesystem path. Must not already exist on disk or in the registry. Cannot be inside `.libra` storage. |

```bash
libra worktree move ../my-feature ../my-feature-v2
libra --json worktree move ../my-feature ../my-feature-v2
```

### Subcommand: `prune`

Remove worktrees from the registry whose directories no longer exist on disk.
Only a path whose stat fails with NotFound counts as missing — a permission
error or an unmounted volume never classifies a worktree as missing. The main
worktree, locked worktrees, tombstone entries (repair's job), and scopes with
an in-progress rebase/cherry-pick/bisect are never pruned. If a pruned
entry's scoped-state cleanup fails, the entry is kept as a `tombstone` for
`libra worktree repair` to retry (reported in the `tombstoned` field).

```bash
libra worktree prune
libra --machine worktree prune
```

### Subcommand: `remove`

Remove a worktree. By default (keep-dir) the directory on disk is
intentionally left untouched — since v0.19.58 this DETACHES the worktree:
the registry entry moves to the `detached_from_registry` state, its scoped
database state (HEAD, index metadata, reflog, layer/sparse/dirty rows) is
preserved, and a marker in the worktree's gitdir makes every command run
inside the directory fail closed with a re-add/delete hint. Re-attach it
with `libra worktree add <path>` (the directory's identity is verified
against the registry's persisted id) or finish the removal with
`--delete-dir`.

Pass `--delete-dir` for Git-style behavior — the directory is removed only
after a dirty-state check passes, the parent directory entry is fsynced,
and only then the scoped database state is cleaned. If that cleanup fails,
a `tombstone` entry remains and `libra worktree repair` retries it. Cannot
remove the main worktree, a locked worktree, or one with an in-progress
rebase/cherry-pick/bisect.

| Argument / Flag | Description |
|-----------------|-------------|
| `<path>` | Filesystem path of the worktree to unregister. |
| `--delete-dir` | After unregistering, also delete the directory on disk. Refused when the worktree contains uncommitted changes (staged or unstaged). |

```bash
# Default — keep the directory on disk
libra worktree remove ../my-feature
libra --json worktree remove ../my-feature

# Git-style — also delete the directory (clean worktree only)
libra worktree remove --delete-dir ../my-feature
libra --machine worktree remove --delete-dir ../my-feature

# Refused when dirty:
$ libra worktree remove --delete-dir ../dirty-feature
fatal: cannot delete dirty worktree '../dirty-feature' (uncommitted changes)
       Hint: commit or stash changes, or remove without --delete-dir to keep the directory
```

Behavior intentionally differs from Git: Git's default deletes the directory.
Libra keeps it by default (as a frozen, re-attachable detached worktree) to
prevent accidental data loss; `--delete-dir` restores Git-like semantics
opt-in. See
[`COMPATIBILITY.md`](../../COMPATIBILITY.md) and
[`compatibility/worktree-surface.md`](../development/commands/worktree.md)
for the rationale.

### Subcommand: `umount`

Unmount a FUSE worktree mountpoint. This is primarily useful for cleaning up
stale Agent task worktrees when the operating system reports a path as busy.
The command also accepts a Libra task worktree root and resolves its
`workspace` mountpoint automatically.

Alias: `unmount`

| Argument / Flag | Description |
|-----------------|-------------|
| `<path>` | FUSE mountpoint path, or a Libra task worktree root containing a `workspace` mountpoint. |
| `--cleanup` | After unmounting, remove the Libra task worktree root. Only task FUSE worktree paths are accepted. |

```bash
libra worktree umount /repo/.libra/worktrees/tasks/libra-task-worktree-fuse-29353-id/workspace --cleanup
libra --json worktree umount /repo/.libra/worktrees/tasks/libra-task-worktree-fuse-29353-id --cleanup
```

JSON / machine output envelope:

```json
{
  "ok": true,
  "command": "worktree.umount",
  "data": {
    "mountpoint": "/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-29353-id/workspace",
    "unmounted": true,
    "cleanup_requested": true,
    "cleanup_root": "/repo/.libra/worktrees/tasks/libra-task-worktree-fuse-29353-id",
    "cleanup_root_removed": true
  }
}
```

### Subcommand: `repair`

Repair worktree metadata. Without an argument, removes duplicate registry entries (same canonical path), ensures exactly one main worktree entry exists, and runs the W3 lifecycle recovery engine: stale intent-journal rows (from an interrupted add/move/remove/prune) are rolled forward or back deterministically (recovery never deletes directories), tombstone entries get their scoped cleanup retried, and detached markers plus the SQL lifecycle mirror are reconciled with the registry. The state file is only rewritten when something actually changed.

With `--migrate-layout`, migrates legacy shared-`.libra` symlink worktrees to the isolated layout (run from the MAIN worktree; `--dry-run` reports without writing; without a path every legacy entry is migrated). The migration installs a fresh journaled gitdir by atomic renames (the legacy link is kept as a backup until verification passes), seeds a DETACHED HEAD at the shared snapshot, and rebuilds the private index from that commit: working files are never touched (they show as dirty/untracked afterwards) and shared STAGED state is never copied — commit or stash it in the main worktree first. An unmerged shared index or an in-progress main rebase/cherry-pick/bisect refuses before any rename; an interrupted migration is recovered by the next plain `worktree repair`.

With a path, restores that **linked** worktree's gitdir identity from the registry (registry v2): rewrites a missing or corrupt `.libra/worktree_id` from the entry's persisted stable id and restores a missing or corrupt (empty/unreadable) `commondir` pointer to this repository's shared storage. The identity always comes from the registry — never from a guess — so the repaired worktree maps back to its own scoped state (HEAD, index, stash snapshots) instead of a fresh scope or the main worktree's. A `commondir` that validly points at a **different** storage is refused (repair never silently re-homes a worktree onto another repository), and the refusal is side-effect free — neither gitdir file is touched. Unregistered paths and the main worktree are refused, and so is a registry still in the legacy v1 format (it carries no persisted identities) — run the no-argument `libra worktree repair` once to upgrade it, then retry.

```bash
libra worktree repair
libra --json worktree repair
libra worktree repair ../experiment
libra --json worktree repair ../experiment
```

## Common Commands

```bash
# Create a new worktree
libra worktree add ../experiment

# List all worktrees
libra wt list

# Lock a worktree to protect it
libra wt lock ../experiment --reason "production hotfix in progress"

# Unlock when done
libra wt unlock ../experiment

# Move a worktree to a new location
libra wt move ../experiment ../experiment-v2

# Clean up worktrees whose directories were deleted
libra wt prune

# Unregister a worktree (keeps files on disk)
libra wt remove ../experiment-v2

# Fix inconsistent worktree metadata
libra wt repair

# Restore a linked worktree's gitdir identity from the registry
libra wt repair ../experiment
```

## Human Output

**`worktree add`**:

```text
/Users/alice/projects/my-feature
```

**`worktree list`**:

```text
main /Users/alice/projects/my-repo
worktree /Users/alice/projects/my-feature
worktree /Users/alice/projects/hotfix [locked: production hotfix in progress]
```

**`worktree remove`**:

```text
Removed worktree '/Users/alice/projects/my-feature' from registry. Directory kept on disk.
Removed worktree '/Users/alice/projects/my-feature' from registry and deleted directory.
```

**`worktree prune`** (with stale entries):

```text
Will prune 2 worktrees:
  /Users/alice/projects/old-experiment
  /Users/alice/projects/deleted-branch
Pruned 2 worktrees
```

**`worktree prune`** (nothing to prune):

```text
No worktrees to prune
```

## JSON Output

`worktree add`, `lock`, `unlock`, `move`, `prune`, `remove`, and `repair`
use command-specific envelopes. `--machine` emits the same schemas as compact
single-line JSON.

**`worktree.add`**:

```json
{
  "ok": true,
  "command": "worktree.add",
  "data": {
    "path": "/Users/alice/projects/my-feature",
    "already_exists": false
  }
}
```

**`worktree.list`**:

```json
{
  "ok": true,
  "command": "worktree.list",
  "data": {
    "worktrees": [
      {
        "kind": "main",
        "path": "/Users/alice/projects/my-repo",
        "is_main": true,
        "locked": false,
        "lock_reason": null,
        "exists": true
      }
    ]
  }
}
```

**`worktree.lock`**:

```json
{
  "ok": true,
  "command": "worktree.lock",
  "data": {
    "path": "/Users/alice/projects/my-feature",
    "locked": true,
    "lock_reason": "long-running experiment",
    "changed": true
  }
}
```

**`worktree.unlock`**:

```json
{
  "ok": true,
  "command": "worktree.unlock",
  "data": {
    "path": "/Users/alice/projects/my-feature",
    "locked": false,
    "changed": true
  }
}
```

**`worktree.move`**:

```json
{
  "ok": true,
  "command": "worktree.move",
  "data": {
    "source": "/Users/alice/projects/my-feature",
    "destination": "/Users/alice/projects/my-feature-v2",
    "registry_updated": true,
    "disk_directory_moved": true
  }
}
```

**`worktree.prune`**:

```json
{
  "ok": true,
  "command": "worktree.prune",
  "data": {
    "pruned": ["/Users/alice/projects/old-experiment"],
    "pruned_count": 1
  }
}
```

**`worktree.remove`**:

```json
{
  "ok": true,
  "command": "worktree.remove",
  "data": {
    "path": "/Users/alice/projects/my-feature",
    "registry_removed": true,
    "disk_directory_deleted": false
  }
}
```

**`worktree.repair`**:

```json
{
  "ok": true,
  "command": "worktree.repair",
  "data": {
    "changed": true,
    "journal_recovered": 1,
    "tombstones_cleaned": 1,
    "tombstones_pending": 0,
    "notes": ["completed interrupted remove of '/abs/path/wt'"]
  }
}
```

**`worktree.repair` with a path**:

```json
{
  "ok": true,
  "command": "worktree.repair",
  "data": {
    "path": "/abs/path/to/experiment",
    "worktree_id": "1f0c…",
    "worktree_id_restored": true,
    "commondir_restored": true
  }
}
```

## Design Rationale

### Why JSON-file persistence instead of filesystem links like Git?

Git tracks worktrees through a combination of filesystem structure: the main `.git/worktrees/` directory contains per-worktree directories with `gitdir`, `HEAD`, and `commondir` files, and each linked worktree has a `.git` file (not directory) pointing back. This approach is tightly coupled to Git's file-based architecture and requires careful cross-referencing between multiple locations.

Libra uses a single `worktrees.json` file in the shared storage directory. This provides several advantages: all worktree metadata is in one queryable location, state is written atomically (via temp-file rename), and the format is trivially inspectable by both humans and AI agents. Each linked worktree's real `.libra` gitdir holds a one-way `commondir` pointer back to the shared storage (plus its stable `worktree_id`), which is simpler than Git's bidirectional pointer system. The trade-off is that the JSON file is a single point of truth that must be kept consistent, which is why `repair` exists.

### Why `--reason` on lock?

Git's `git worktree lock` also supports `--reason`, and Libra preserves this. Lock reasons are valuable in team environments and when AI agents manage worktrees: they provide context about why a worktree should not be pruned or removed. Without a reason, a locked worktree is opaque, and another user (or agent) cannot determine whether the lock is still relevant. The reason is displayed in `list` output, making lock status self-documenting.

### Why does `remove` not delete directories on disk?

Deleting files is a destructive operation that cannot be undone. Libra's default `remove` DETACHES the worktree instead: the directory, its scoped database state, and its identity stay intact while the entry moves to `detached_from_registry` and the directory is frozen (every command inside it fails closed with a re-add/delete hint). This is a deliberate safety choice: the user can re-attach with `worktree add`, or finish the removal with `--delete-dir` once confident nothing is needed. It also prevents accidental data loss if a worktree contains uncommitted work. Git's `git worktree remove` deletes the directory by default, which has been a source of lost work.

### Why does `move` reject locked worktrees?

A locked worktree signals that it should not be modified. Moving it would change its filesystem path, which could break references to that path in other tools, scripts, or agent configurations. The user must explicitly unlock the worktree before moving it, ensuring the action is intentional.

### Why does `add` populate from HEAD instead of the index?

When creating a linked worktree, Libra restores content from the HEAD commit rather than the current index state. This ensures the new worktree reflects the last committed state, not any staged-but-uncommitted changes that exist only in the original worktree's context. This matches user expectations: a new worktree starts from a known good state.

## Parameter Comparison: Libra vs Git vs jj

| Operation | Libra | Git | jj |
|-----------|-------|-----|----|
| Create worktree | `worktree add <path> [<branch-or-commit>]` (no target: detached at source commit) | `worktree add <path> [<branch>]` | `workspace add <path>` |
| Create on branch | `worktree add <path> <branch>` (attached; refused if checked out anywhere; no DWIM) | `worktree add <path> <branch>` | `workspace add <path>` (then `jj edit`) |
| Create detached | `worktree add --detach <path> [<commit>]` (also `add <path> <commit>`) | `worktree add --detach <path> <commit>` | N/A |
| List worktrees | `worktree list` | `worktree list [--porcelain]` | `workspace list` |
| Lock | `worktree lock <path> [--reason]` | `worktree lock [--reason] <worktree>` | N/A |
| Unlock | `worktree unlock <path>` | `worktree unlock <worktree>` | N/A |
| Move | `worktree move <src> <dest>` | `worktree move <worktree> <new-path>` | N/A |
| Prune | `worktree prune` | `worktree prune [--dry-run]` | N/A (automatic) |
| Remove | `worktree remove <path>` (detaches — directory + state kept, frozen) | `worktree remove [--force] <worktree>` (deletes dir) | `workspace forget <name>` |
| Repair | `worktree repair [<path>]` | `worktree repair [<path>...]` | N/A |
| Alias | `wt` | N/A | N/A |
| Branch per worktree | `-b <new> [<start>]` explicit (full rollback; no basename default) | Automatic (new branch or existing) | Automatic (new working copy commit) |
| Storage | JSON file (`worktrees.json`) | Filesystem structure (`.git/worktrees/`) | Operation log |
| Worktree link | Real local `.libra` gitdir with a `commondir` pointer to shared storage (legacy layouts: symlink) | `.git` file pointing to `gitdir` | Symlink to shared `.jj` |

Note: jj uses the term "workspace" instead of "worktree". Each workspace automatically gets its own working copy commit, and workspaces are tracked in the operation log. jj workspaces are simpler than Git worktrees because jj's change-based model does not require separate branch management per workspace.

## Error Handling

| Code | Condition |
|------|-----------|
| `LBR-REPO-001` | Not a libra repository |
| `LBR-REPO-002` | `worktrees.json` is corrupt |
| `LBR-CLI-003` | Worktree path cannot be inside `.libra` storage |
| `LBR-CLI-003` | Target exists and is not a directory |
| `LBR-CLI-003` | No such worktree (for lock, unlock, move, remove) |
| `LBR-CLI-003` | Cannot move or remove main worktree |
| `LBR-CLI-003` | Cannot move or remove locked worktree |
| `LBR-CLI-003` | `worktree umount --cleanup` was requested for a non-task FUSE worktree path |
| `LBR-CONFLICT-002` | Target directory exists and is not empty |
| `LBR-CONFLICT-002` | Target already contains a `.libra` entry |
| `LBR-CONFLICT-002` | Destination already exists (for move) |
| `LBR-CONFLICT-002` | Destination already registered as worktree (for move) |
| `LBR-CONFLICT-002` | `--delete-dir` refused because the worktree is dirty |
| `LBR-IO-001` | Failed to read or inspect worktree paths/state/status |
| `LBR-IO-002` | Failed to write worktrees.json |
| `LBR-IO-002` | Failed to populate worktree from HEAD |
