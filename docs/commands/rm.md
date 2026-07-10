# `libra rm`

Remove files from the working tree and/or the index.

**Aliases:** `remove`, `delete`

## Synopsis

```
libra rm [--json|--machine] <pathspec>...
libra rm [--json|--machine] --cached <pathspec>...
libra rm [--json|--machine] -r <pathspec>...
libra rm [--json|--machine] --dry-run <pathspec>...
```

## Description

`libra rm` removes files from the working tree and the index. By default it deletes the file from disk and unstages it so the removal is recorded in the next commit. With `--cached`, only the index entry is removed and the file remains on disk -- useful for untracking a file that was added by mistake without losing local changes.

Removing a directory requires the `-r` (recursive) flag. Without it,
specifying a directory path produces an error. With `-r`, Libra removes only
the tracked files matched by the pathspec and prunes directories only after
they become empty; untracked files and excluded tracked files keep their
directories on disk.

Pathspecs use Libra's shared Git-style matcher: plain pathspecs match a file or
directory prefix, wildcard pathspecs are supported, and the high-value magic
forms `:(top)`, `:/`, `:(glob)`, `:(literal)`, `:(icase)`,
`:(exclude)`, `:!`, and `:^` are honored. Exclude pathspecs subtract from the
positive selection, and pathspec matching follows `core.ignorecase` when enabled.
Wildcard-looking pathspecs also match an exact path or directory prefix with
the same literal text, matching Git's bracket-file and bracket-directory
behavior.

Before removing a file, Libra checks for uncommitted changes (both staged and unstaged). If the file has local modifications relative to the index, or the index differs from HEAD, the command refuses to proceed unless `--force` is passed or `--cached` is used. This safety check prevents silent data loss when a file has unsaved work.

Aliases: `remove`, `delete`. All three names invoke the same command.

## Options

| Flag | Short | Long | Description |
|------|-------|------|-------------|
| Pathspec | | positional | One or more files or directories to remove. Supports shared pathspec magic; required unless `--pathspec-from-file` is used. |
| Cached | | `--cached` | Only remove from the index; keep the working tree file intact. |
| Recursive | `-r` | `--recursive` | Allow recursive removal when a directory is specified. |
| Force | `-f` | `--force` | Force removal, bypassing the uncommitted-changes safety check. |
| Dry run | | `--dry-run` | Show what would be removed without actually deleting anything. |
| Ignore unmatch | | `--ignore-unmatch` | Exit with zero status even if no pathspec matched any file. |
| Pathspec from file | | `--pathspec-from-file <FILE>` | Read shared-matcher pathspecs from a file, one per line. |
| NUL separator | | `--pathspec-file-nul` | Pathspec file entries are separated by NUL bytes instead of newlines. |
| Sparse | | `--sparse` | Accepted for Git compatibility as a no-op. Git uses it to allow removing index entries outside the sparse-checkout cone; Libra has no sparse-checkout state, so it changes nothing. |

### Option Details

**`--cached`**

Unstages the file but leaves the working tree copy in place. After running `libra rm --cached secret.env`, the file disappears from the index (and will show as "deleted" in the next commit), but the file remains on disk. This is the standard way to stop tracking a file without deleting it.

```bash
$ libra rm --cached config/local.toml
rm 'config/local.toml'
```

**`-f` / `--force`**

Bypasses safety checks for files with uncommitted changes. Normally Libra refuses to remove a file when:
1. The working tree version differs from the index (local modifications).
2. The index version differs from HEAD (staged changes).
3. Both conditions are true simultaneously.

With `--force`, the file is removed regardless.

**`--dry-run`**

Shows what would be removed without touching the filesystem or index:

```bash
$ libra rm --dry-run src/old_module.rs tests/old_test.rs
rm 'src/old_module.rs'
rm 'tests/old_test.rs'
```

**`--pathspec-from-file`**

Reads shared-matcher pathspecs from a file instead of command-line arguments.
Combined with `--pathspec-file-nul`, this supports filenames containing
newlines or other special characters:

```bash
$ libra rm --pathspec-from-file files-to-remove.txt
$ libra rm --pathspec-from-file files.txt --pathspec-file-nul
```

## Common Commands

```bash
# Remove a single file from both index and disk
libra rm src/deprecated.rs

# Untrack a file but keep it on disk
libra rm --cached .env

# Recursively remove a directory
libra rm -r old_module/

# Preview what would be removed
libra rm --dry-run -r build/
libra --json rm --dry-run -r build/

# Force remove a file with local modifications
libra rm -f src/experimental.rs

# Remove files listed in a manifest
libra rm --pathspec-from-file cleanup-list.txt

# Remove tracked Rust files from the index except generated output
libra rm --cached ':(glob)src/*.rs' ':(exclude)src/generated.rs'

# Remove from index, ignore if file is not tracked
libra rm --cached --ignore-unmatch generated.rs
```

## Human Output

Each removed file is reported on its own line:

```text
rm 'src/deprecated.rs'
rm 'old_module/foo.rs'
rm 'old_module/bar.rs'
```

In `--dry-run` mode, the same output is produced but no files are modified.

Global `--quiet` suppresses this primary human output while keeping warnings
and errors on stderr.

## JSON Output

`--json` and `--machine` use the `rm` command envelope. `paths` contains every
tracked file matched for index removal. `directories` contains recursive plain
directory pathspecs and reports whether the directory actually disappeared from
disk after tracked files were deleted and empty parents were pruned. In
`--dry-run`, the same candidate paths are reported, but `removed_from_index`
and `removed_from_disk` are `false`.

```json
{
  "ok": true,
  "command": "rm",
  "data": {
    "pathspecs": ["src/deprecated.rs"],
    "paths": [
      {
        "path": "src/deprecated.rs",
        "removed_from_index": true,
        "removed_from_disk": true
      }
    ],
    "directories": [],
    "cached": false,
    "recursive": false,
    "forced": false,
    "dry_run": false
  }
}
```

`--machine` emits the same envelope as compact single-line JSON.

## Design Rationale

### Why aliases `remove` and `delete`?

`rm` is terse and familiar to Git users, but not self-documenting. `remove` reads naturally in scripts and documentation. `delete` matches the vocabulary many developers reach for first. Supporting all three names reduces friction without adding any implementation complexity -- they are clap aliases that map to the same handler.

### Why `--pathspec-from-file`?

When removing many files programmatically (e.g., a CI cleanup step or a migration script), command-line argument limits can be hit. `--pathspec-from-file` avoids this by reading paths from a file. The `--pathspec-file-nul` variant handles pathnames with spaces or newlines safely, following the same convention as `git rm --pathspec-from-file`.

### Why safety checks on uncommitted changes?

Removing a file that has local modifications silently destroys work. Git requires `--force` in the same scenario. Libra follows this convention exactly: if the working tree differs from the index or the index differs from HEAD, the command errors with a message explaining which flag to use (`--cached` to keep the file, `-f` to force deletion). This two-flag escape hatch lets users express intent clearly.

### Why no per-command `--quiet` flag?

Unlike `libra clean`, the `rm` command does not have a command-specific quiet
flag. Use the global `--quiet` flag to suppress primary stdout while preserving
warnings and errors.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Basic remove | `libra rm <path>` | `git rm <path>` | `jj file untrack <path>` |
| Cache only | `--cached` | `--cached` | Default (jj untrack only affects tracking) |
| Recursive | `-r` / `--recursive` | `-r` / `--recursive` | Implicit (jj untrack handles dirs) |
| Force | `-f` / `--force` | `-f` / `--force` | Not needed (no safety check) |
| Dry run | `--dry-run` | `--dry-run` / `-n` | Not available |
| Ignore unmatch | `--ignore-unmatch` | `--ignore-unmatch` | Not available |
| Pathspec from file | `--pathspec-from-file` | `--pathspec-from-file` | Not available |
| NUL separator | `--pathspec-file-nul` | `--pathspec-file-nul` | Not available |
| Sparse | `--sparse` (accepted no-op) | `--sparse` | Not available |
| Quiet | Global `--quiet` | `-q` / `--quiet` | Not available |
| Aliases | `rm`, `remove`, `delete` | `rm` only | `file untrack` |

Note: jj's `file untrack` is conceptually similar to `libra rm --cached` -- it stops tracking a file without deleting it. jj does not have a command that both untracks and deletes a file in one step.

## Error Handling

| Scenario | Behavior | Exit |
|----------|----------|------|
| No pathspecs provided | Error: nothing specified for removal | non-zero |
| Path not found in index | Error (or zero with `--ignore-unmatch`) | non-zero / 0 |
| Directory without `-r` | Error: not removing directory recursively without `-r` | non-zero |
| Uncommitted local modifications | Error: file has local modifications, use `--cached` or `-f` | non-zero |
| Staged changes differ from HEAD | Error: file has staged changes, use `--cached` or `-f` | non-zero |
| Both staged and local changes | Error: file has staged content different from both the file and HEAD, use `-f` | non-zero |
| Not inside a repository | Error: repository not found | non-zero |
