# `libra status`

Show the working tree status.

**Alias:** `st`

## Synopsis

```
libra status [OPTIONS] [pathspec]...
```

## Description

`libra status` shows the state of the working tree and staging area: which files are staged
for the next commit, which have modifications not yet staged, and which are untracked. It also
reports the current branch, detached HEAD state, and upstream tracking information.

The command computes the diff between HEAD, the index, and the working tree to classify files
into staged, unstaged, and untracked categories. It supports multiple output formats: a
human-readable long format (default, also selectable explicitly with `--long`), a short format (`--short`), a machine-readable porcelain
format, structured JSON for agent consumption, and `-z` NUL-terminated machine output. It can
also detect renames (`--find-renames`), align output into columns (`--column`), and control
whether upstream ahead/behind counts are shown (`--ahead-behind` / `--no-ahead-behind`).
Optional pathspecs limit the reported staged, unstaged, unmerged, ignored, and
untracked paths. They use the shared pathspec engine, including `:(top)`,
`:(exclude)`, `:(icase)`, `:(literal)`, and `:(glob)` magic.
An in-progress merge is still reported as a global repository state even when
the selected pathspec hides every conflicted path; `--exit-code` remains dirty
until the merge is continued or aborted.

During merge, rebase, and cherry-pick conflicts, unmerged index entries are reported as conflicts
instead of untracked files. Porcelain v1/short output uses Git-style XY codes such as `UU
conflict.txt`; porcelain v2 emits `u <XY> ...` records with stage modes and object IDs.

Tracked symlinks participate in the same HEAD/index/worktree comparison as
regular files. `status` treats the symlink itself as the worktree object,
compares the stored link target bytes, and reports target changes as
modifications instead of following the link or treating dangling symlinks as
deleted.

## Options

### `<pathspec>...`

Limit status output to matching paths. Pathspecs resolve from the current
working directory unless `:(top)` is used, and support exact files, directory
prefixes, default wildcards, and `:(top)` / `:(exclude)` / `:(icase)` /
`:(literal)` / `:(glob)` magic.

### `-s, --short`

Give the output in the short format. Each file is shown on a single line with a two-character
status code (e.g., `M ` for staged modified, ` M` for unstaged modified, `??` for untracked).
Conflicts with `--porcelain`.

```bash
libra status -s
libra status --short
```

### `--porcelain [VERSION]`

Output in a machine-readable format. Accepts an optional version argument: `v1` (default) or
`v2` for extended format. Conflicts with `--short`.

```bash
libra status --porcelain
libra status --porcelain v1
libra status --porcelain v2
```

### `--branch` (`-b`)

Include branch information in short or porcelain output. Shows the current branch and its
tracking relationship on the first line. `-b` is the short alias, so `libra status -sb`
matches `git status -sb`.

```bash
libra status --short --branch
libra status -sb
libra status --porcelain --branch
```

### `--ahead-behind` / `--no-ahead-behind`

Control whether ahead/behind counts are shown in the branch tracking line. `--no-ahead-behind`
suppresses the counts while still showing the upstream branch name. The default is to show the
counts when an upstream is configured.

```bash
libra status --short --branch --no-ahead-behind
libra status --porcelain --branch --no-ahead-behind
```

### `-z`

Terminate each machine-readable status entry with a NUL (`\0`) byte instead of a newline. This
is intended for use with `--porcelain` or `--short` so that paths containing spaces or newlines
can be parsed reliably.

```bash
libra status --porcelain -z
libra status -s -z
```

### `--column`

Align human-readable status entries into columns. In staged/unstaged sections, status labels
(`modified:`, `deleted:`, `new file:`, `renamed:`) are padded to the same width. In untracked
and ignored sections, file names are laid out in multiple columns.

```bash
libra status --column
```

### `--no-column`

Do not align status entries into columns (equivalent to `--column=never`),
countermanding an earlier `--column` (last one on the command line wins). Status
is not columnar by default, so on its own this is a no-op.

```bash
libra status --no-column
```

### `--find-renames [PERCENT]`

Detect renames among staged and unstaged changes. When a deleted file and a new file have the
same blob hash, or their file names are sufficiently similar, they are reported as a rename
pair (`old -> new`) instead of separate delete/add entries. The optional value is the minimum
similarity percentage (0-100); the default is 50.

```bash
libra status --find-renames
libra status --find-renames=75
```

### `--renames` / `--no-renames`

Toggle rename detection. `--renames` enables it at the default (or `--find-renames`)
threshold; `--no-renames` disables it and overrides `--renames`/`--find-renames` when
combined.

```bash
libra status --renames
libra status --no-renames
```

Show the number of stash entries. Only effective in standard (long) output mode.

```bash
libra status --show-stash
```

### `--scan` / `--cached` / `--check-dirty` (Libra extensions, lore.md 1.1)

`--scan` runs the normal full status AND atomically rebuilds the dirty-set
cache from it (TOCTOU-guarded on the index fingerprint + HEAD; a scan lock
blocks concurrent scanners, stale locks are stolen). `--cached` consumes the
cache instead of walking the worktree — O(dirty paths); any freshness doubt
degrades to the full status with a hint. Snapshot semantics: worktree-only
edits made after the scan are invisible until a rescan or a `libra dirty`
mark (that is what the marks are for). NOTE: unrelated to Git's `--cached`
(= the index). `--check-dirty` re-verifies only the cached set, pruning rows
proven clean. The three are mutually exclusive and conflict with
`--porcelain`/`--short`/`--ignored`; default `status` never touches the
cache and its JSON gains no keys. See [dirty.md](dirty.md).

### `--ignored`

Include ignored files in the output.

```bash
libra status --ignored
```

### `-u, --untracked-files [<MODE>]`

Control how untracked files are displayed. Accepted values: `normal` (default, shows untracked
directories but not their contents), `all` (recursively lists files within untracked directories),
`no` (hides untracked files entirely). As in Git, the flag with no value means `all`, and the short
form takes an attached value (`-uno`, `-uall`, `-unormal`).

```bash
libra status -uno                  # hide untracked files
libra status -u                    # same as -uall (recurse into untracked dirs)
libra status --untracked-files=all
```

### `--exit-code`

Exit with code 1 if the working tree has changes, exit 0 if clean. Useful for scripting
and CI pipelines to detect dirty state without parsing output.

```bash
libra status --exit-code
libra status --quiet --exit-code   # silent dirty check
```

## Common Commands

```bash
libra status
libra status --short
libra status --porcelain -z
libra status --column
libra status --find-renames
libra status --json
libra status --exit-code
```

## Human Output

Default human mode writes the status summary to `stdout`.

Clean working tree:

```text
On branch main
nothing to commit, working tree clean
```

With changes:

```text
On branch main
Your branch is ahead of 'origin/main' by 2 commits.
  (use "libra push" to publish your local commits)

Changes to be committed:
        new file:   src/feature.rs
        modified:   src/lib.rs

Changes not staged for commit:
        modified:   README.md

Untracked files:
        notes.txt
```

Detached HEAD:

```text
HEAD detached at abc1234
nothing to commit, working tree clean
```

Short format (`--short`):

```text
A  src/feature.rs
M  src/lib.rs
 M README.md
?? notes.txt
```

Unmerged conflict:

```text
UU conflict.txt
```

`--quiet` suppresses all `stdout` output. Combined with `--exit-code`, it acts as a
silent dirty check (exit 1 if dirty, exit 0 if clean).

## Structured Output

`libra status` supports the global `--json` and `--machine` flags.

- `--json` writes one success envelope to `stdout`
- `--machine` writes the same schema as compact single-line JSON
- `stderr` stays clean on success

Example:

```json
{
  "ok": true,
  "command": "status",
  "data": {
    "head": {
      "type": "branch",
      "name": "main"
    },
    "has_commits": true,
    "upstream": {
      "remote_ref": "origin/main",
      "ahead": 2,
      "behind": 0,
      "gone": false
    },
    "staged": {
      "new": ["src/feature.rs"],
      "modified": ["src/lib.rs"],
      "deleted": []
    },
    "unstaged": {
      "modified": ["README.md"],
      "deleted": []
    },
    "untracked": ["notes.txt"],
    "ignored": [],
    "is_clean": false
  }
}
```

Clean working tree:

```json
{
  "ok": true,
  "command": "status",
  "data": {
    "head": {
      "type": "branch",
      "name": "main"
    },
    "has_commits": true,
    "upstream": null,
    "staged": {
      "new": [],
      "modified": [],
      "deleted": []
    },
    "unstaged": {
      "modified": [],
      "deleted": []
    },
    "untracked": [],
    "ignored": [],
    "is_clean": true
  }
}
```

Detached HEAD:

```json
{
  "ok": true,
  "command": "status",
  "data": {
    "head": {
      "type": "detached",
      "oid": "abc1234def5678..."
    },
    "has_commits": true,
    "upstream": null,
    "staged": { "new": [], "modified": [], "deleted": [] },
    "unstaged": { "modified": [], "deleted": [] },
    "untracked": [],
    "ignored": [],
    "is_clean": true
  }
}
```

### Schema Notes

- `head.type` is `"branch"` or `"detached"`
- When on a branch, `head.name` is the branch name; when detached, `head.oid` is the commit hash
- `upstream` is `null` when no tracking branch is configured or HEAD is detached
- `upstream.gone` is `true` when the remote tracking branch no longer exists
- `upstream.ahead` / `upstream.behind` are `null` when `gone` is `true`
- `is_clean` is `true` only when staged, unstaged, untracked, and unmerged
  lists are empty and no global merge state is active
- `has_commits` is `false` in a freshly initialized repository with no commits
- `stash_entries` (optional, integer): present only when `--show-stash` is
  passed. Counts the entries on the stash stack (matching `libra stash list`)
  and may be `0`. Omitted entirely without `--show-stash` so JSON consumers
  can distinguish "stash subsystem not queried" from "stash subsystem
  queried, returned zero" — i.e. the field's *presence* signals an
  explicit opt-in, not the existence of stashed work.

## Design Rationale

### Porcelain v1 and v2

`libra status --porcelain` (no version) emits Git's classic v1 short-format
layout (`XY <path>` per file). `libra status --porcelain v2` emits the
extended v2 line layout — for each tracked file:

```text
1 XY <sub> <mode_HEAD> <mode_index> <mode_worktree> <hash_HEAD> <hash_index> <path>
```

Untracked entries collapse to `? <path>` and ignored entries to `! <path>`,
matching Git's own v2 encoding. The implementation lives in
`src/command/status.rs::output_porcelain_v2` and is fed by
`build_porcelain_v2_data`, which pulls mode + hash metadata out of the
index and HEAD tree before rendering.

With `-z`, porcelain v1 and v2 records are NUL-terminated and contain no
trailing newlines. Rename-capable porcelain output does not use the human
`old -> new` arrow form under `-z`; scripts should split fields on NUL.

Most consumers should still prefer `--json` (or `--machine` for compact
single-line JSON): the JSON envelope carries the same staged/unstaged/
untracked partitioning plus upstream tracking and `stash_entries`, and
is far easier to parse than v2's positional text columns. Use
`--porcelain v2` only when you specifically need Git-compatible output
for tooling that already speaks the v2 grammar.

### Explicit `--exit-code` instead of implicit behavior

Git's `git status` always exits 0 regardless of repository state, and checking for dirty state
requires `git diff --exit-code` or parsing `git status --porcelain` output. Libra adds an
explicit `--exit-code` flag that returns exit 1 when the working tree is dirty. This is
intentionally opt-in (rather than default) to avoid breaking scripts that check `$?` after
`libra status`. Combined with `--quiet`, it provides a zero-output, exit-code-only dirty check
that is cleaner than parsing text output.

### `--show-stash` in standard mode only

The `--show-stash` flag only affects the long (standard) human-readable output, not short or
porcelain formats. This matches Git's behavior where `--show-stash` appends a stash summary
line to the long format. In JSON output, stash information could be added to the envelope in a
future iteration without needing a separate flag, since JSON consumers can simply ignore fields
they do not need.

### Enhanced upstream tracking info in JSON

Git's porcelain v1 does not include upstream tracking information; porcelain v2 adds a header
line with ahead/behind counts. Libra's JSON output always includes a full `upstream` object
with `remote_ref`, `ahead`, `behind`, and `gone` fields when a tracking branch is configured.
This rich upstream data is critical for AI agents and CI tools that need to determine whether
a branch needs to be pushed or pulled, without having to run separate `libra log` or
`libra branch -vv` commands.

## Parameter Comparison: Libra vs Git vs jj

| Parameter / Flag | Git | jj | Libra |
|---|---|---|---|
| Show status | `git status` | `jj status` / `jj st` | `libra status` |
| Long format | `git status --long` (default) | N/A | `libra status --long` (default) |
| Short format | `git status -s` / `--short` | N/A (always short) | `libra status -s` / `--short` |
| Porcelain v1 | `git status --porcelain` | N/A | `libra status --porcelain` |
| Porcelain v2 | `git status --porcelain=v2` | N/A | `libra status --porcelain v2` (v1 semantics) |
| Branch info in short | `git status -sb` | Always shown | `libra status -sb` (`--short --branch`) |
| Show stash count | `git status --show-stash` | N/A | `libra status --show-stash` (standard mode) |
| Show ignored files | `git status --ignored` | N/A | `libra status --ignored` |
| Untracked files control | `git status -u<mode>` | N/A (always shows) | `libra status -u<mode>` / `--untracked-files=<mode>` |
| Exit code for dirty | `git diff --exit-code` | N/A | `libra status --exit-code` |
| Quiet mode | `git status -q` | N/A | `libra status --quiet` (global flag) |
| Column display | `git status --column` | N/A | `libra status --column` (`--no-column` countermands) |
| Ahead/behind display | `git status -sb` (text only) | N/A | Human + structured `upstream` object in JSON |
| Find renames | `git status -M` | Automatic | `--find-renames` / `--renames` |
| Ignore submodules | `git status --ignore-submodules` | N/A | N/A (no submodules) |
| Structured JSON output | N/A | N/A | `--json` / `--machine` |
| Error hints | Minimal | Minimal | Every error type has an actionable hint |

## Exit Code Behavior

| Flag | Clean | Dirty |
|------|-------|-------|
| (default) | exit 0 | exit 0 |
| `--exit-code` | exit 0 | exit 1 |

`--exit-code` enables a silent dirty check useful for scripting. When combined with
`--quiet`, no output is produced -- only the exit code signals the repository state.

## Error Handling

Every `StatusError` variant maps to an explicit `StableErrorCode`.

| Scenario | Error Code | Exit | Hint |
|----------|-----------|------|------|
| Index file corrupted | `LBR-REPO-002` | 128 | "the index file may be corrupted" |
| Invalid path encoding | `LBR-CLI-003` | 129 | "path contains invalid characters" |
| Failed to hash a file | `LBR-IO-001` | 128 | -- |
| Cannot list working directory | `LBR-IO-001` | 128 | -- |
| Working directory not found | `LBR-REPO-001` | 128 | -- |
| Bare repository | `LBR-REPO-003` | 128 | "this operation must be run in a work tree" |

## Compatibility Notes

- `--porcelain v2` is accepted but currently produces v1-format output; use `--json` for full structured data
- jj's `jj status` always uses a short format and does not distinguish staged from unstaged changes (jj has no staging area)
- Rename detection is supported via `--find-renames[=<n>]` and the `--renames`/`--no-renames` toggles; Git's short `-M` alias is not exposed
- `--column` column-aligned display is supported; `--no-column` (equivalent to `--column=never`) countermands an earlier `--column` via clap's symmetric override (last one wins), and status is not columnar by default so `--no-column` alone is a no-op
