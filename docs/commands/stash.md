# `libra stash`

Stash the changes in a dirty working directory away.

## Synopsis

```
libra stash push [-m <message>] [-u | -a] [-k | --keep-index] [-- <pathspec>...]
libra stash pop [<stash>]
libra stash list
libra stash apply [<stash>]
libra stash drop [<stash>]
libra stash show [<stash>] [-p | --patch] [--name-only | --name-status]
libra stash branch <branch> [<stash>]
libra stash clear [--force]
```

## Description

`libra stash` saves your local modifications to a new stash entry and reverts the working directory to match HEAD. By default, `stash push` records tracked index/worktree changes and leaves untracked files alone. Use `-u` / `--include-untracked` to include visible untracked files, or `-a` / `--all` to include ignored files too. Pass `-- <pathspec>...` (file or directory paths; `.` selects the whole tree) to stash only the changes to those paths, leaving every other change in the working tree. A pathspec cannot be combined with `-u`/`-a`/`-k`. The modifications can be restored later with `libra stash pop` or `libra stash apply`, which replay the stash onto the CURRENT working tree (not HEAD) — so any unrelated uncommitted change you made in the meantime, including the paths a pathspec push left behind, is preserved. Default `apply` / `pop` leave the current index intact, so restored tracked changes appear as unstaged working-tree changes; Git's `--index` restore mode is not exposed yet. If `stash push` is run on a clean working tree and no requested untracked files exist, it exits successfully as a no-op and reports that there are no local changes to save.

Stash entries are stored as specially-structured commit objects under `.libra/refs/stash`, with a flat-file list tracking the stash stack. Each stash captures both the index state and worktree state at the time of creation.

## Options

### Subcommands

#### `push`

Save your local modifications to a new stash and clean the working directory.

| Option | Short | Long | Description |
|--------|-------|------|-------------|
| Message | `-m` | `--message` | Optional descriptive message for the stash entry. If omitted, a default "WIP on `<branch>`: `<short-hash>` ..." message is generated. |
| Include untracked | `-u` | `--include-untracked` | Include visible untracked files in the stash and remove them from the worktree. Ignored files remain in place. |
| No include untracked | | `--no-include-untracked` | Do not include untracked files (the default), countermanding an earlier `-u`/`--include-untracked` (last one wins). Untracked files are excluded by default, so on its own this is a no-op. |
| Include all | `-a` | `--all` | Include visible untracked and ignored files in the stash, then remove them from the worktree. |
| Keep index | `-k` | `--keep-index` | Keep staged changes in the index and restore the worktree to the staged content, removing only unstaged deltas. |

```bash
# Save with default message
libra stash push

# Save with a descriptive message
libra stash push -m "work in progress on feature X"

# Include visible untracked files
libra stash push -u

# Include ignored files too
libra stash push -a

# Stash only unstaged deltas while keeping staged content ready to commit
libra stash push --keep-index

# Stash only the changes to specific paths (here a file and a directory),
# leaving every other change in the working tree
libra stash push -- src/main.rs docs/
```

#### `pop`

Apply the top stash entry and remove it from the stash list. Equivalent to `apply` followed by `drop`. By default, restored tracked changes are written to the working tree only and are not staged.

| Argument | Description |
|----------|-------------|
| `<stash>` | Stash reference, e.g. `stash@{1}`. Defaults to `stash@{0}` (the most recent stash). |

```bash
# Pop the latest stash
libra stash pop

# Pop a specific stash
libra stash pop stash@{2}
```

#### `list`

List all stash entries with their index, message, and stash ID.

```bash
libra stash list
```

#### `apply`

Apply a stash entry without removing it from the stash list. Useful when you want to apply the same stash to multiple branches. By default, restored tracked changes are written to the working tree only and are not staged.

| Argument | Description |
|----------|-------------|
| `<stash>` | Stash reference, e.g. `stash@{1}`. Defaults to `stash@{0}`. |

```bash
libra stash apply
libra stash apply stash@{1}
```

#### `drop`

Remove a single stash entry from the stash list without applying it.

| Argument | Description |
|----------|-------------|
| `<stash>` | Stash reference, e.g. `stash@{1}`. Defaults to `stash@{0}`. |

```bash
libra stash drop
libra stash drop stash@{1}
```

#### `show`

Show the file-level changes recorded in a stash entry.

| Argument / Flag | Description |
|-----------------|-------------|
| `<stash>` | Stash reference, e.g. `stash@{1}`. Defaults to `stash@{0}`. |
| `-p` / `--patch` | Show the stashed changes as a unified diff (patch) instead of the file-level summary. |
| `--name-only` | Show only the changed file names, one per line. |
| `--name-status` | Show file names prefixed with the status code (`A` / `M` / `D`). |

`--name-only` and `--name-status` are mutually exclusive in human render mode; the JSON envelope always carries the full `files` list with status, regardless of which hint is set. With `-p`/`--patch`, the human output is the unified diff (no summary footer) and the JSON envelope adds a `patch` field (absent otherwise).

```bash
# File-level summary of stash@{0}
libra stash show

# Inspect a specific stash entry
libra stash show stash@{1}

# Show the stashed changes as a unified diff
libra stash show -p

# File names only
libra stash show --name-only
```

#### `branch`

Create a new branch from a stash entry, apply the stash on it, then drop the entry. Useful when a stash applies cleanly only on a branch that no longer exists, or when you want to resume the stashed work as a normal branch.

| Argument | Description |
|----------|-------------|
| `<branch>` | Name of the new branch to create. Required. |
| `<stash>` | Stash reference, e.g. `stash@{1}`. Defaults to `stash@{0}`. |

```bash
# Branch off the latest stash and drop it
libra stash branch hotfix

# Branch off a specific stash
libra stash branch hotfix stash@{2}
```

#### `clear`

Remove every stash entry. Outside `--json` / `--machine` mode, `--force` is required to prevent accidental data loss.

| Flag | Description |
|------|-------------|
| `--force` | Skip the confirmation requirement. Mandatory in human mode; bypassed automatically in JSON / machine mode. |

```bash
# Human mode (refuses without --force)
libra stash clear --force

# JSON mode (--force not required)
libra stash clear --json
```

### Global Flags

| Flag | Description |
|------|-------------|
| `--json` | Emit structured JSON output |
| `--quiet` | Suppress human-readable output |

## Common Commands

```bash
# Save current changes
libra stash push

# Save with a message
libra stash push -m "work in progress on feature X"

# Save tracked changes plus visible untracked files
libra stash push -u

# Save unstaged deltas without disturbing staged content
libra stash push --keep-index

# List stashes
libra stash list

# Apply and remove the latest stash
libra stash pop

# Apply without removing
libra stash apply

# Drop a specific stash
libra stash drop stash@{1}

# JSON output for scripting
libra stash list --json
```

## Human Output

**`stash push`** (with changes):

```text
Saved working directory and index state WIP on main: abc1234 ...
```

**`stash push`** (clean working tree):

```text
No local changes to save
```

**`stash list`**:

```text
stash@{0}: WIP on main: abc1234 initial commit
stash@{1}: On main: work in progress on feature X
```

**`stash pop` / `stash apply`**:

```text
On branch main
Changes restored from stash@{0}
```

**`stash drop`**:

```text
Dropped stash@{0} (abc1234...)
```

## Structured Output (JSON)

When `--json` is passed, all subcommands produce a JSON envelope:

```json
{
  "command": "stash",
  "data": {
    "action": "push",
    "message": "WIP on main: abc1234 ...",
    "stash_id": "..."
  }
}
```

When `-u`, `-a`, or `--keep-index` is used, the push envelope adds only the relevant fields:

```json
{
  "command": "stash",
  "data": {
    "action": "push",
    "message": "WIP on main: abc1234 ...",
    "stash_id": "...",
    "included_untracked": 2,
    "kept_index": true
  }
}
```

On a clean working tree, `stash push --json` returns:

```json
{
  "command": "stash",
  "data": { "action": "noop", "message": "No local changes to save" }
}
```

The `data.action` field is one of: `noop`, `push`, `pop`, `apply`, `drop`, `list`, `show`, `branch`, `clear`.

### `list` JSON schema

```json
{
  "command": "stash",
  "data": {
    "action": "list",
    "entries": [
      { "index": 0, "message": "WIP on main: ...", "stash_id": "abc1234..." }
    ]
  }
}
```

### `pop` / `apply` JSON schema

```json
{
  "command": "stash",
  "data": {
    "action": "pop",
    "index": 0,
    "stash_id": "abc1234...",
    "branch": "main"
  }
}
```

### `drop` JSON schema

```json
{
  "command": "stash",
  "data": {
    "action": "drop",
    "index": 0,
    "stash_id": "abc1234..."
  }
}
```

### `show` JSON schema

```json
{
  "command": "stash",
  "data": {
    "action": "show",
    "stash": "stash@{0}",
    "stash_id": "abc1234...",
    "files": [
      { "path": "src/foo.rs", "status": "M" }
    ],
    "files_changed": {
      "total": 1,
      "added": 0,
      "modified": 1,
      "deleted": 0
    }
  }
}
```

The structured envelope always emits the full `files` list. The `--name-only` / `--name-status` flags only affect human render output.

### `branch` JSON schema

```json
{
  "command": "stash",
  "data": {
    "action": "branch",
    "branch": "hotfix",
    "stash": "stash@{0}",
    "stash_id": "abc1234...",
    "applied": true,
    "dropped": true
  }
}
```

### `clear` JSON schema

```json
{
  "command": "stash",
  "data": {
    "action": "clear",
    "cleared_count": 3
  }
}
```

## Design Rationale

### How untracked and ignored files are stored

`stash push -u` and `stash push -a` use a third stash parent for the untracked/all snapshot, matching Git's object topology. `stash apply` and `stash pop` restore those files as untracked worktree files. If a local file would be overwritten during restore, the apply/pop operation fails and keeps the stash entry intact.

### How `pop` / `apply` treat the index

Default `stash pop` and `stash apply` restore tracked content to the working tree while leaving the current index unchanged. A change that was staged when stashed comes back as an unstaged working-tree edit unless a future `--index` mode is added. This matches Git's default `stash pop` / `stash apply` behavior and prevents a later `libra commit` from committing restored stash content before the user runs `libra add`.

### How `--keep-index` works

`stash push --keep-index` stores the same stash metadata as a normal push, then writes the saved index back and restores the worktree to the index state. For a mixed file with both staged and unstaged edits, the staged content remains in the index and worktree, while the unstaged delta is saved in the stash.

### Why a curated subcommand model?

Git's stash has grown organically and supports `git stash` as a shorthand for `git stash push`, plus `git stash save` (deprecated) and the plumbing pair `git stash create` / `git stash store`. Libra exposes the eight subcommands users actually reach for in practice: `push`, `pop`, `list`, `apply`, `drop`, `show`, `branch`, and `clear`. The plumbing pair (`create` / `store`) and the `save` shorthand are deferred — see [`docs/development/commands/_compatibility.md`](../development/commands/_compatibility.md) sections D8 and D9. This keeps the surface aligned with stock Git for everyday workflows while leaving rarely-used plumbing out of the maintained surface.

### Why `stash@{N}` syntax instead of plain indices?

Libra preserves Git's `stash@{N}` reference syntax for familiarity. Users migrating from Git can use the same muscle memory. The parser also accepts bare integers in some contexts, but the canonical form remains `stash@{N}`.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Push (save changes) | `stash push` | `stash push` / `stash save` (deprecated) | N/A (no stash; use `jj new` to shelve) |
| Message | `-m <message>` | `-m <message>` | N/A |
| Keep index | `--keep-index` | `--keep-index` / `--no-keep-index` | N/A |
| Include untracked | `-u` / `--include-untracked` | `-u` / `--include-untracked` | N/A |
| No include untracked | `--no-include-untracked` (countermands `-u`) | `--no-include-untracked` | N/A |
| Include all (ignored too) | `-a` / `--all` | `-a` / `--all` | N/A |
| Pathspec (partial stash) | `stash push -- <pathspec>...` (file/dir paths, `.` = whole tree; not combinable with `-u`/`-a`/`-k` → `LBR-CLI-002`; no match → `LBR-CLI-003`) | `stash push [--] <pathspec>...` | N/A |
| Pop | `stash pop [ref]` | `stash pop [--index] [<stash>]` | N/A |
| Apply | `stash apply [ref]` | `stash apply [--index] [<stash>]` | N/A |
| Drop | `stash drop [ref]` | `stash drop [<stash>]` | N/A |
| List | `stash list` | `stash list [<log-options>]` | N/A |
| Show file-level summary | `stash show [<stash>] [--name-only \| --name-status]` | `stash show [<stash>]` | N/A |
| Show stash as a patch | `stash show -p \| --patch [<stash>]` | `stash show -p [<stash>]` | N/A |
| Create branch from stash | `stash branch <branch> [<stash>]` | `stash branch <branch> [<stash>]` | N/A |
| Clear all stashes | `stash clear [--force]` | `stash clear` | N/A |
| Plumbing create/store | Not supported (deferred — see compatibility/declined.md D8/D9) | `stash create` / `stash store` | N/A |
| JSON output | `--json` | Not supported | N/A |
| Quiet mode | `--quiet` | `-q` / `--quiet` | N/A |

Note: jj does not have a stash command. Its change-based model allows creating anonymous changes with `jj new` that serve a similar purpose to stashing.

## Error Handling

| Code | Condition |
|------|-----------|
| `LBR-REPO-001` | Not a libra repository |
| `LBR-REPO-003` | No initial commit |
| `LBR-CLI-002` | Invalid stash reference syntax |
| `LBR-CLI-003` | Stash does not exist |
| `LBR-CONFLICT-001` | Merge conflict during stash apply |
