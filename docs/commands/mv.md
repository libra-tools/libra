# `libra mv`

Move or rename files and directories.

## Synopsis

```
libra mv [<options>] <source>... <destination>
```

## Description

`libra mv` moves or renames files and directories in the working tree and updates the index accordingly. The last argument is always the destination; all preceding arguments are sources. When there are multiple sources, the destination must be an existing directory.

The command validates that all source paths exist, are tracked in the index, are not in a conflicted state, and reside within the repository working directory. With `-k` / `--skip-errors`, invalid source candidates are skipped and valid candidates continue. Directory moves are performed as a single filesystem rename, with individual index entries updated for each tracked file within the directory. Untracked files inside a moved directory are carried along by the filesystem rename but are not added to the index.

After all filesystem moves succeed, the index is updated atomically: old entries are removed and new entries (with recalculated blob hashes) are inserted. The index is saved only after all operations complete successfully.

## Options

| Flag | Short | Long | Description |
|------|-------|------|-------------|
| Verbose | `-v` | `--verbose` | Print each rename operation as it happens. |
| Dry run | `-n` | `--dry-run` | Show what would be moved without actually performing any moves. |
| Force | `-f` | `--force` | Overwrite an existing destination file instead of reporting an error. Only works for regular files and symlinks; directories cannot be overwritten. |
| Skip errors | `-k` | `--skip-errors` | Skip invalid source candidates and move the remaining valid candidates. |
| Sparse | | `--sparse` | Accept Git's sparse-checkout flag as a no-op because Libra does not maintain sparse-checkout state. |

### Option Details

**`-v` / `--verbose`**

Prints each rename operation during execution:

```bash
$ libra mv -v old.rs new.rs
Renaming old.rs to new.rs
```

**`-n` / `--dry-run`**

Previews the rename operations without performing them. No objects are written and no operation is recorded:

```bash
$ libra mv -n old.rs new.rs
Checking rename of 'old.rs' to 'new.rs'
Renaming old.rs to new.rs
```

No filesystem changes or index updates are made in dry-run mode.

**`-f` / `--force`**

Allows overwriting an existing destination. Without this flag, moving to an existing path is an error:

```bash
$ libra mv -f src/old.rs src/new.rs
```

**`-k` / `--skip-errors`**

Skips source candidates that would fail pre-validation and continues with the remaining valid candidates:

```bash
$ libra mv -k missing.rs tracked.rs src/
```

If every source is skipped, the command exits successfully without changing the worktree or index, matching Git's `mv -k` behavior. Repository-boundary errors and multi-source moves to a non-directory destination remain fatal.

**`--sparse`**

Accepted for Git CLI compatibility. Libra has no sparse-checkout cone state, so the flag does not change move planning, filesystem writes, index updates, or structured output.

## Case-only renames (lore.md 1.14)

`libra mv Foo foo` is a first-class case-only rename: on a case-insensitive
filesystem the destination resolves to the source itself (same inode), which
libra detects (device+inode) and renames in place — no `--force` needed, and
the force-remove branch (which would have deleted the file's only copy) is
bypassed. Directories work too (`mv Dir dir` renames instead of nesting).
Related: `core.casehandling` (`error` default / `warn` / `allow`) governs
implicit case collisions in `add`/`checkout`/`switch`; `core.ignorecase` is
probed and recorded truthfully at `init` on every platform.

## Common Commands

```bash
# Rename a file
libra mv old_name.rs new_name.rs

# Move a file into a directory
libra mv utils.rs src/

# Move multiple files into a directory
libra mv a.rs b.rs c.rs src/

# Move a directory into another directory
libra mv old_dir/ parent_dir/

# Preview what would happen
libra mv -n old.rs new.rs

# Force overwrite
libra mv -f src/draft.rs src/final.rs

# Skip invalid sources
libra mv -k missing.rs tracked.rs src/

# Accept Git sparse flag as a no-op
libra mv --sparse old.rs new.rs

# Verbose output
libra mv -v old.rs new.rs
```

## Human Output

Normal move (no flags):

```text
(no output)
```

Verbose mode:

```text
Renaming old.rs to new.rs
```

Dry-run mode:

```text
Checking rename of 'old.rs' to 'new.rs'
Renaming old.rs to new.rs
```

Global `--quiet` suppresses dry-run and verbose human output while keeping
warnings and errors on stderr.

## Structured Output

`libra mv` supports the global `--json` and `--machine` flags on successful moves.

- `--json` writes one success envelope to `stdout`
- `--machine` writes the same schema as compact single-line JSON
- `stderr` stays clean on success
- dry-run output reports the planned move pairs without changing the filesystem or index
- `moves` / `index_updates` list only the source candidates that are actually planned or moved
- `-k` / `--skip-errors` adds a `skipped` array — one `{ "source", "reason" }` entry per source it dropped (e.g. a missing or untracked source). The field is omitted when nothing was skipped. Human mode stays silent on skips (matching Git's `mv -k`); the detail is JSON-only.
- `--sparse` is a no-op and does not add a `sparse` field

Example:

```json
{
  "ok": true,
  "command": "mv",
  "data": {
    "moves": [
      {
        "source": "old.rs",
        "destination": "new.rs"
      }
    ],
    "index_updates": [
      {
        "source": "old.rs",
        "destination": "new.rs"
      }
    ],
    "dry_run": false,
    "forced": false,
    "verbose": false
  }
}
```

Dry-run:

```json
{
  "ok": true,
  "command": "mv",
  "data": {
    "moves": [
      {
        "source": "old.rs",
        "destination": "new.rs"
      }
    ],
    "index_updates": [
      {
        "source": "old.rs",
        "destination": "new.rs"
      }
    ],
    "dry_run": true,
    "forced": false,
    "verbose": false
  }
}
```

Skipped sources (`-k` / `--skip-errors`):

```json
{
  "ok": true,
  "command": "mv",
  "data": {
    "moves": [
      {
        "source": "tracked.rs",
        "destination": "src/tracked.rs"
      }
    ],
    "index_updates": [
      {
        "source": "tracked.rs",
        "destination": "src/tracked.rs"
      }
    ],
    "dry_run": false,
    "forced": false,
    "verbose": false,
    "skipped": [
      {
        "source": "missing.rs",
        "reason": "bad source, source=missing.rs, destination=src"
      }
    ]
  }
}
```

## Design Rationale

### Why paths-based instead of explicit `--source` / `--dest`?

Libra follows the same convention as Git's `mv` and the Unix `mv` command: the last argument is the destination, and all preceding arguments are sources. This is familiar to every Unix user and avoids the verbosity of named flags for what is fundamentally a positional operation.

The trade-off is that the command requires at least two arguments and the semantics change depending on whether the destination is an existing directory. This is the same trade-off that Unix `mv` and Git `mv` make, and decades of usage have shown it to be intuitive in practice.

### Why is `--sparse` a no-op?

Git's `mv` supports `--sparse` to allow moving files outside the sparse-checkout cone. Libra does not yet implement sparse checkout state, so there is no cone membership to relax. The flag is accepted to keep Git-compatible scripts working, but it intentionally leaves normal repository-boundary validation unchanged.

### Why validate tracked status?

Unlike a plain filesystem `mv`, `libra mv` refuses to move files that are not tracked in the index. This prevents confusion where a user moves a file expecting the rename to be recorded in version control, but the file was never tracked in the first place. If you need to move an untracked file, use the system `mv` command.

### Why refuse conflicted files?

Moving a file that is in a conflicted state (stages 1-3 in the index) would lose conflict information. Libra requires conflicts to be resolved before the file can be moved.

### How does this compare to Git and jj?

Git's `mv` command is similar in design: it moves files in the working tree and updates the index. Libra supports the common Git flags, including `-k` / `--skip-errors`; `--sparse` is accepted as a no-op until Libra gains sparse-checkout state.

jj does not have a `mv` command. Because jj uses automatic snapshotting of the working tree, file moves are detected automatically by the working-copy scanner. Users simply move files with the system `mv` command and jj records the change on the next snapshot. This works well for simple renames but cannot reliably detect moves (as opposed to delete-then-create) for large refactors.

Libra provides an explicit `mv` command (like Git) because its index-based model requires explicit notification of renames to maintain accurate tracking.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Source paths | `<source>...` (positional) | `<source>...` (positional) | N/A (use system `mv`) |
| Destination | Last positional argument | Last positional argument | N/A |
| Verbose | `-v` / `--verbose` | `-v` / `--verbose` | N/A |
| Dry run | `-n` / `--dry-run` | `-n` / `--dry-run` | N/A |
| Force overwrite | `-f` / `--force` | `-f` / `--force` | N/A |
| Structured JSON output | `--json` / `--machine` | N/A | N/A |
| Skip errors | `-k` / `--skip-errors` | `-k` | N/A |
| Sparse checkout | `--sparse` accepted as no-op | `--sparse` | N/A |

Note: jj does not have a dedicated mv command. File renames are detected automatically by the working-copy snapshot mechanism.

## Error Handling

| Scenario | Error Message |
|----------|---------------|
| Fewer than 2 arguments | Usage information printed |
| Source does not exist | `fatal: bad source, source=<src>, destination=<dst>` |
| Source is the same as destination | `fatal: can not move directory into itself` |
| Multiple sources with non-directory destination | `fatal: destination '<dst>' is not a directory` |
| Source not tracked in index | `fatal: not under version control, source=<src>, destination=<dst>` |
| Source has merge conflicts | `fatal: conflicted, source=<src>, destination=<dst>` |
| Destination exists without `--force` | `fatal: destination already exists, source=<src>, destination=<dst>` |
| Directory destination already has source name | `fatal: destination already exists, source=<src>, destination=<dst>` |
| Path outside repository | `fatal: '<path>' is outside of the repository at '<workdir>'` |
| Multiple sources targeting the same path | `fatal: multiple sources moving to the same target path` |
| Invalid source with `-k` | Source is skipped; command succeeds if no fatal repository-boundary or destination-shape error occurs |
| Filesystem rename failed | `fatal: failed to move, source=<src>, destination=<dst>, error=<err>` |
| Index save failed | `fatal: failed to save index after mv: <err>` |

mv renames a directory to a new nonexistent path.

Moving to a path whose parent traverses a symbolic link is refused with `LBR-CLI-003` and `destination is beyond a symbolic link`, leaving the index unchanged (also under `-f`).
