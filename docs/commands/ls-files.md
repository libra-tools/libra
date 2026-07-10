# `libra ls-files`

List tracked index entries and untracked working-tree paths.

## Synopsis

```bash
libra ls-files [OPTIONS] [pathspec]...
```

## Description

`libra ls-files` reads Libra's index and working tree and prints repository
paths without mutating refs, the index, the worktree, or object storage.
With no state filter it defaults to the cached index view, so tracked paths
remain listed even when the working tree copy is modified or deleted.

This public compatibility slice supports cached listing, modified/deleted
filters, stage-style output, untracked listing, Git/Libra ignore-source
filtering via `--others --exclude-standard`, ignored-only listing via `-i`/`--ignored`
(`-i -o` for ignored untracked files, `-i -c` for tracked files matching an
exclude pattern), repository-local pathspec filtering, `--error-unmatch`,
NUL-delimited text output via `-z`, status tags via `-t`, and unmerged-only
listing via `-u` / `--unmerged`. `--full-name` is accepted as a no-op (Libra
always prints repo-root-relative paths).

Tracked symlinks are inspected as symlinks for `--deleted` and `--modified`:
a dangling symlink still exists and is not listed as deleted, while a changed
link target is listed as modified.

Pathspecs are resolved from the caller's current working directory, not forced
to the repository root. Exact-file and directory-prefix filtering are both
supported, and the shared pathspec engine also accepts `:(top)`, `:(exclude)`,
`:(icase)`, `:(literal)`, and `:(glob)` magic. Pathspecs that resolve outside
the repository are rejected. The resolve-undo and sparse-checkout integration
remain deferred.

When stdout is piped and the downstream command exits early, `libra ls-files` exits quietly
without printing panic/backtrace or `Broken pipe` diagnostics.

## Options

| Option | Description |
|--------|-------------|
| `--cached` | Show cached index entries. This is the default when no state filter is provided. |
| `--deleted`, `-d` | Show tracked paths whose working-tree file is missing. |
| `--modified`, `-m` | Show tracked paths whose working-tree content hash differs from the index. |
| `--stage` | Print stage-style records, including conflict stages when present. |
| `-s` | Short alias for stage-style output: `<mode> <object> <stage>\t<path>`. |
| `--abbrev[=<n>]` | Abbreviate the object name to `<n>` hex digits in `-s`/`--stage` output. Bare `--abbrev` uses 7; `--abbrev=<n>` sets the length (the value requires the `=` form, so bare `--abbrev` never consumes a following pathspec). Libra truncates to a fixed length rather than computing the shortest unique prefix. |
| `-t` | Prefix each path with a status tag: `H` (cached), `R` (removed/deleted), `C` (modified/changed), `?` (other/untracked), `M` (unmerged). Unmerged paths are not hidden; each stage 1/2/3 entry is printed as `M <path>`, matching Git's conflict visibility. |
| `-u`, `--unmerged` | Show only unmerged (conflict) entries — index stages 1/2/3 — in stage-style output. |
| `--full-name` | Accepted for Git compatibility. Libra always prints repo-root-relative paths (the `git --full-name` form), so this is a no-op. |
| `--others`, `-o` | Show untracked working-tree files. |
| `--cached`, `-c` | Show files staged in the index. |
| `-i`, `--ignored` | Show only the ignored set: `-i -o` lists ignored untracked files (the inverse of `-o`), `-i -c` lists tracked files matching an exclude pattern. Must be combined with `-o`/`-c` and needs an exclude source — `--exclude-standard` or an explicit `-x`/`-X` pattern (exit 128 otherwise), matching Git. |
| `--exclude-standard` | With `--others`, honor standard Git/Libra ignore sources (`.gitignore`, `.git/info/exclude`, `core.excludesFile`, and `.libraignore`). |
| `-x`, `--exclude <pattern>` | Skip untracked files matching `<pattern>` (gitignore syntax) from the `--others` listing. Repeatable; supplements `--exclude-standard`. With `-i` the pattern instead defines the ignored set. |
| `-X`, `--exclude-from <file>` | Read additional exclude patterns from `<file>` (one per line; `#` comments and blank lines skipped) and apply them like `-x`. Repeatable. |
| `--error-unmatch` | Exit 1 with `LBR-CLI-003` if any explicit pathspec matches no files in the selected result set. |
| `--eol` | Prefix each cached entry with `i/<eol> w/<eol> attr/<attr>` line-ending info: `<eol>` is `lf`/`crlf`/`mixed`/`none`/`-text` (binary) for the index blob (`i/`) and the worktree file (`w/`). Byte-compatible with `git ls-files --eol`; `attr/` is currently empty because line-ending attribute reporting is not implemented. |
| `-z` | Emit NUL-delimited text records instead of newline-delimited output. Text mode only; rejects `--json` / `--machine`. |
| `<pathspec>...` | Limit output to matching paths. Supports exact files, directory prefixes, default wildcards, and `:(top)` / `:(exclude)` / `:(icase)` / `:(literal)` / `:(glob)` magic. Pathspecs resolve from the current working directory unless `:(top)` is used. |
| `--json` | Emit the standard Libra JSON envelope. |
| `--machine` | Emit the same envelope as one compact JSON line. |

## Examples

```bash
libra ls-files
libra ls-files --modified
libra ls-files --deleted
libra ls-files --others
libra ls-files --others --exclude-standard
libra ls-files -o -x '*.log'              # untracked files except *.log
libra ls-files -o -X .extra-excludes      # read extra exclude patterns from a file
libra ls-files -i -o --exclude-standard   # only the ignored untracked files
libra ls-files tracked-dir
libra ls-files --others --exclude-standard others-dir
libra ls-files --error-unmatch src/lib.rs
libra ls-files -z tracked-dir
libra ls-files --stage
libra ls-files -t
libra ls-files -t --others --exclude-standard
libra ls-files -u
libra --json ls-files --modified
```

## Human Output

Default output prints one repository path per line:

```text
.libraignore
tracked.txt
```

`--stage` and `-s` print Git-style stage records:

```text
100644 4f3c2d1a7b8c9d0e1234567890abcdef12345678 0	tracked.txt
```

Unmerged entries are visible both as stage rows and as tagged rows:

```text
100644 1111111111111111111111111111111111111111 1	conflict.txt
100644 2222222222222222222222222222222222222222 2	conflict.txt
100644 3333333333333333333333333333333333333333 3	conflict.txt
M conflict.txt
M conflict.txt
M conflict.txt
```

`-z` keeps the same record shape but terminates each record with NUL instead of
newline, which is useful for shell-safe scripting:

```text
tracked-dir/alpha.txt\0tracked-dir/bravo.txt\0
```

## Structured Output

`--json` and `--machine` use the standard Libra command envelope. Each entry in
`data` includes `path`, `hash`, `mode`, `stage`, and `status`. Untracked
entries use `null` for fields that do not apply:

```json
{
  "ok": true,
  "command": "ls-files",
  "data": [
    {
      "path": "tracked.txt",
      "hash": "4f3c2d1a7b8c9d0e1234567890abcdef12345678",
      "mode": "100644",
      "stage": 0,
      "status": "modified"
    },
    {
      "path": "untracked.txt",
      "hash": null,
      "mode": null,
      "stage": null,
      "status": "other"
    }
  ]
}
```

## Parameter Comparison: Libra vs Git vs Jujutsu

| Feature | Libra | Git | Jujutsu |
|---------|-------|-----|---------|
| Cached index listing | Default / `--cached` | Default / `--cached` | Use status/file commands |
| Modified tracked files | `-m` / `--modified` | `-m` / `--modified` | Use status/diff commands |
| Deleted tracked files | `-d` / `--deleted` | `-d` / `--deleted` | Use status commands |
| Stage-style output | `--stage` / `-s` | `--stage` / `-s` | Different model |
| Abbreviate object name | `--abbrev[=<n>]` (fixed-length) | `--abbrev[=<n>]` (shortest unique) | N/A |
| Untracked files | `--others` | `--others` | Use status/file commands |
| Ignore-aware untracked | `--others --exclude-standard` | Same | Different model |
| Ignored files only | `-i -o --exclude-standard` | Same (`-i -c` for tracked) | Different model |
| Explicit exclude pattern | `-x` / `--exclude <pattern>` | `-x` / `--exclude` | Different model |
| Explicit exclude file | `-X` / `--exclude-from <file>` | `-X` / `--exclude-from` | Different model |
| Pathspec filters | `<pathspec>...` | Supported | Different model |
| Unmatched pathspec failure | `--error-unmatch` | `--error-unmatch` | Different model |
| Line-ending info | `--eol` (`attr/` always empty) | `--eol` | N/A |
| NUL output | `-z` (text mode only) | `-z` | Different model |
| Status tags | `-t` (H/R/C/?/M) | `-t` (H/S/M/R/C/K/?) | Different model |
| Unmerged entries | `-u` / `--unmerged` | `-u` / `--unmerged` | Different model |
| Root-relative paths | `--full-name` (always; no-op flag) | `--full-name` (opt-in) | Different model |
