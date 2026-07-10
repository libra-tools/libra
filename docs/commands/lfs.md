# `libra lfs`

Manage Large File Storage for binary and media assets.

## Synopsis

```
libra lfs track [<pattern>...]
libra lfs untrack <path>...
libra lfs locks [--id <ID>] [--path <PATH>] [--limit <N>]
libra lfs lock <path>
libra lfs unlock <path> [--force] [--id <ID>]
libra lfs ls-files [--long] [--size] [--name-only]
```

## Description

`libra lfs` provides built-in Large File Storage for managing binary files, media assets, and other large objects that do not diff or merge well. Instead of storing the full file content in the repository, LFS replaces large files with lightweight pointer files and stores the actual content on a dedicated LFS server.

LFS tracking is read from Git/Libra attribute sources (`core.attributesFile`,
per-directory `.gitattributes`, `.libra_attributes`, and
`.git/info/attributes`) when `add` and `lfs ls-files` decide whether a path has
`filter=lfs`. The `track` and `untrack` subcommands still manage the root
`.libra_attributes` file as Libra's writable convenience layer. File locking prevents concurrent edits to
binary files that cannot be merged, with server-side enforcement via the LFS
lock API.

Unlike Git, which requires a separate `git-lfs` extension installed as a smudge/clean filter, Libra integrates LFS natively. The LFS client, pointer file parsing, and attribute management are built into the `libra` binary. No additional installation or filter configuration is needed.

## Options

`libra lfs` has no top-level options. All functionality is accessed through subcommands documented below.

## Subcommands

### `track`

View or add LFS tracking patterns to Libra Attributes.

```bash
# List currently tracked patterns
libra lfs track

# Track all PNG files
libra lfs track "*.png"

# Track multiple patterns
libra lfs track "*.psd" "*.zip" "assets/**"
```

| Argument | Description |
|----------|-------------|
| `<pattern>...` | Optional glob patterns to add. If omitted, lists existing tracked patterns. |

When called without arguments, prints each tracked pattern and the attributes file it was found in:

```text
Listing tracked patterns
    *.png (.libra_attributes)
    *.psd (.libra_attributes)
```

When called with patterns, appends them to the root `.libra_attributes` file, creating the file if it does not exist.

### `untrack`

Remove LFS tracking patterns from Libra Attributes.

```bash
libra lfs untrack "*.png"
```

| Argument | Description |
|----------|-------------|
| `<path>...` | One or more patterns to remove from `.libra_attributes`. |

Removes exact matches of the specified patterns from the attributes file. Files already committed as LFS pointers remain as pointers until re-added normally.

### `locks`

List files currently locked on the LFS server for the current branch.

```bash
# List all locks
libra lfs locks

# Filter by path
libra lfs locks --path assets/logo.png

# Filter by lock ID
libra lfs locks --id 12345

# Limit results
libra lfs locks --limit 10
```

| Flag | Short | Long | Description |
|------|-------|------|-------------|
| ID | `-i` | `--id` | Filter by lock ID. |
| Path | `-p` | `--path` | Filter by file path. |
| Limit | `-l` | `--limit` | Maximum number of locks to return. |

Output format:

```text
assets/logo.png    ID:12345
docs/spec.pdf      ID:12346
```

### `lock`

Lock a file on the LFS server to prevent concurrent edits.

```bash
libra lfs lock assets/logo.png
```

| Argument | Description |
|----------|-------------|
| `<path>` | Path to the file to lock, relative to the repository root. |

The file must exist in the working tree. On success, prints `Locked <path>`. Locking requires push access to the repository.

### `unlock`

Remove a lock from a file on the LFS server.

```bash
# Unlock by path
libra lfs unlock assets/logo.png

# Force unlock (skip working tree check)
libra lfs unlock assets/logo.png --force

# Unlock by ID
libra lfs unlock assets/logo.png --id 12345
```

| Flag | Short | Long | Description |
|------|-------|------|-------------|
| Force | `-f` | `--force` | Skip file existence and working-tree cleanliness checks. |
| ID | `-i` | `--id` | Unlock by lock ID instead of looking up the ID from the path. |

Without `--force`, the command verifies that the file exists and the working tree is clean before unlocking. With `--force`, these checks are bypassed -- useful for unlocking files that have been deleted or when the working tree is intentionally dirty.

### `ls-files`

Show information about LFS-tracked files in the index.

```bash
# Default output (short OID, pointer status)
libra lfs ls-files

# Show full 64-character OID
libra lfs ls-files --long

# Include file size
libra lfs ls-files --size

# Show only filenames
libra lfs ls-files --name-only
```

| Flag | Short | Long | Description |
|------|-------|------|-------------|
| Long | `-l` | `--long` | Show the entire 64-character OID instead of the first 10 characters. |
| Size | `-s` | `--size` | Show the LFS object size in parentheses at the end of each line. |
| Name only | `-n` | `--name-only` | Show only the tracked file names, without OID or status. |

Output uses `*` after the OID to indicate a full (smudged) object and `-` to indicate an LFS pointer:

```text
a1b2c3d4e5 * assets/logo.png
f6g7h8i9j0 - docs/spec.pdf
```

## JSON / Machine Output

`--json` and `--machine` are supported for successful `track`, `untrack`, `locks`, `lock`, `unlock`, and `ls-files` operations. `--json` writes one command envelope to stdout, and `--machine` emits the same envelope as a compact single JSON line.

Tracking patterns:

```json
{
  "ok": true,
  "command": "lfs",
  "data": {
    "action": "track",
    "patterns": ["*.png"]
  }
}
```

Listing LFS files:

```json
{
  "ok": true,
  "command": "lfs",
  "data": {
    "action": "ls-files",
    "show_size": true,
    "files": [
      {
        "path": "assets/logo.png",
        "oid": "a1b2c3d4e5",
        "marker": "-",
        "size": 1024,
        "display_size": " (1.00 KiB)"
      }
    ]
  }
}
```

Lock operations include `path`, `id` when available, `refspec`, or a `locks` array for `lfs locks`.

## Lock enforcement (`lfs.lockEnforce`, lore.md 2.8)

An opt-in policy gate on `libra add` / `libra commit` against LFS locks held
by **someone else** (your own locks never warn or block):

```bash
libra config lfs.lockEnforce warn    # warn and proceed
libra config lfs.lockEnforce block   # refuse before anything is staged
libra config lfs.lockEnforce off     # explicit off (overrides a broader setting)
```

The LFS server stays the single source of truth (`locks/verify`, the same
check `push` performs); ownership is decided server-side from your
authenticated user. Staged deletions are covered too — they never reach the
push-time check. With `block`, an unreachable server fails closed (use
`--offline` to skip deliberately, or downgrade to `warn`); explicit offline
intent skips with a recorded warning in both modes. A server without a
locking API (404) is a clean no-op. Previews (`add --dry-run`,
`commit --porcelain`) never touch the network.

## Common Commands

```bash
# Set up LFS tracking for common binary types
libra lfs track "*.png" "*.jpg" "*.gif" "*.pdf" "*.zip"

# Check what is being tracked
libra lfs track

# See all LFS files with sizes
libra lfs ls-files --size

# Lock a file before editing
libra lfs lock assets/hero-image.psd

# Check current locks
libra lfs locks

# Unlock after committing changes
libra lfs unlock assets/hero-image.psd

# Stop tracking a pattern
libra lfs untrack "*.gif"
```

## Design Rationale

### Why built-in LFS instead of a separate extension?

Git LFS is a separate binary that hooks into Git via smudge/clean filters and a custom transfer agent. This architecture has several pain points:
- **Installation friction**: Every developer must install `git-lfs` and run `git lfs install` to configure filters. Forgetting this step silently commits pointer files as regular blobs.
- **Filter misconfiguration**: Smudge/clean filter setup is fragile. A `.gitattributes` typo or missing filter config leads to corrupted checkouts where pointer files appear instead of content.
- **Transfer complexity**: Git LFS intercepts `git push`/`git pull` via pre-push hooks and custom transfer protocols, adding failure modes that are difficult to debug.

Libra integrates LFS at the binary level: the pointer format, attribute parsing, batch API client, and lock management are all compiled in. `libra add` automatically detects LFS-tracked patterns and creates pointer files. `libra checkout` automatically smudges pointers back to full content. No hooks, no filters, no separate installation.

### Why file locking?

Binary files (PSDs, compiled assets, large datasets) cannot be merged. When two developers edit the same binary file, one of them will lose their work on merge. File locking provides server-side coordination: `libra lfs lock` claims exclusive edit rights, and `libra lfs unlock` releases them. The `locks` subcommand lets developers see who has locked what before starting work.

The `--force` flag on `unlock` is an escape hatch for administrators to release stale locks (e.g., when the lock holder is on vacation or has left the team).

### Why check working-tree cleanliness on unlock?

Unlocking a file while the working tree is dirty could indicate that the developer has uncommitted LFS changes that would be lost if someone else immediately locks and modifies the file. The cleanliness check is a safety reminder to commit before releasing the lock. `--force` bypasses this for cases where the dirty state is unrelated to the locked file.

## Parameter Comparison: Libra vs Git (git-lfs) vs jj

| Parameter | Libra | Git (git-lfs) | jj |
|-----------|-------|---------------|-----|
| Track patterns | `libra lfs track <pattern>` | `git lfs track <pattern>` | Not available |
| Untrack patterns | `libra lfs untrack <pattern>` | `git lfs untrack <pattern>` | Not available |
| List tracked patterns | `libra lfs track` (no args) | `git lfs track` (no args) | Not available |
| List locks | `libra lfs locks` | `git lfs locks` | Not available |
| Lock a file | `libra lfs lock <path>` | `git lfs lock <path>` | Not available |
| Unlock a file | `libra lfs unlock <path>` | `git lfs unlock <path>` | Not available |
| Force unlock | `--force` | `--force` | Not available |
| List LFS files | `libra lfs ls-files` | `git lfs ls-files` | Not available |
| Long OID | `--long` | `--long` | Not available |
| File size | `--size` | `--size` | Not available |
| Name only | `--name-only` | `--name-only` | Not available |
| Installation required | Built-in | Separate `git-lfs` install + `git lfs install` | Not available |
| Attributes file | Reads `.gitattributes` + `.libra_attributes`; `track` writes `.libra_attributes` | `.gitattributes` | Not available |
| Filter configuration | Automatic | Manual (smudge/clean filters) | Not available |

Note: jj does not currently have LFS support. Large file management in jj repositories requires using Git's LFS infrastructure via jj's Git backend.

## Error Handling

| Scenario | StableErrorCode | Description |
|----------|-----------------|-------------|
| `lock` on non-existent path | `CliInvalidTarget` | The specified file does not exist in the working tree. |
| `lock` without push access | `AuthPermissionDenied` | The user lacks push permissions on the repository. |
| `lock` on already-locked file | `ConflictOperationBlocked` | A lock already exists for the specified path. |
| `unlock` on non-existent path (no `--force`) | `CliInvalidTarget` | The specified file does not exist. |
| `unlock` with dirty working tree (no `--force`) | `ConflictOperationBlocked` | The working tree has uncommitted changes. |
| `unlock` on file with no lock | `RepoStateInvalid` | No lock was found for the specified path. |
| `unlock` without push access | `AuthPermissionDenied` | The user lacks push permissions. |
| Failed to read/write `.libra_attributes` | IO error | The writable Libra attributes file could not be read or written. |
| Failed to load index | IO error | The repository index is corrupted or missing. |
| LFS server communication failure | Network error | The LFS server returned an unexpected status code. |
