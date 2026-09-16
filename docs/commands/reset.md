# `libra reset`

Move `HEAD` and reset the index or working tree depending on the selected mode.

## Synopsis

```
libra reset [<target>] [--soft | --mixed | --hard | --merge | --keep]
libra reset <pathspec>...
libra reset [<target>] [--] <pathspec>...
libra reset [<target>] --pathspec-from-file=<file> [--pathspec-file-nul]
```

## Description

`libra reset` moves the HEAD reference to a target commit and optionally resets the index and working tree to match. Five modes control how much state is affected:

- **`--soft`**: moves HEAD only. The index and working tree are untouched, so all differences between the old HEAD and the target appear as staged changes. Useful for squashing commits.
- **`--mixed`** (default): moves HEAD and resets the index. The working tree is untouched, so changes appear as unstaged modifications. Useful for un-staging files.
- **`--hard`**: moves HEAD, resets the index, and restores the working tree. All uncommitted changes are discarded. Useful for fully reverting to a known state.
- **`--merge`**: resets HEAD/index and updates paths changed between the old HEAD and target, while preserving unstaged changes whose index entry already matches the target. It refuses before mutation when a target/index change would overwrite an unstaged path.
- **`--keep`**: resets HEAD/index and updates paths changed between the old HEAD and target, but refuses when any such path has staged or unstaged local changes. Local changes on unaffected paths are preserved.

Both preserving modes perform a complete preflight before writing. Their exact original index bytes and affected worktree entries are snapshotted; a worktree or final ref-update failure restores both snapshots, so a failed reset does not partially move or rewrite the repository. Worktree traversal never follows a symlink ancestor (including an ignored symlink): unsafe writes fail and rollback without touching the link target. Corrupt index/tree paths that escape the worktree or target `.libra` metadata are rejected. `--merge` also carries forward existing unmerged index stages.

When `--hard` restores a tree entry whose mode is `120000`, Libra creates a
real symlink on Unix from the stored link target bytes. If a regular file
currently occupies that path, it is replaced by the symlink; if a symlink
currently occupies a path that should become a regular file, the link itself is
removed before writing. Platforms without symlink support report an explicit
unsupported diagnostic.

When pathspecs are provided, the command performs a targeted mixed reset: only the named files are reset in the index to match the target commit, without moving HEAD. This is the primary way to un-stage specific files. Like Git, a bare first positional that is a known path and not a revision is treated as a pathspec with target `HEAD`, so `libra reset src/lib.rs` is equivalent to `libra reset HEAD -- src/lib.rs`. If a token is both a revision and a filename, reset refuses it as ambiguous; use `libra reset <revision> -- <file>` for a target revision or `libra reset -- <file>` for a path. Pathspecs are incompatible with `--soft`, `--hard`, `--merge`, and `--keep`. When a pathspec reset restores a symlink from the target commit, the index entry keeps mode `120000` and the blob remains the link target bytes.

A whole-tree reset (any mode, no pathspecs) also ends a stopped sequence item: it clears a stopped single-commit cherry-pick or revert, so the next pick or revert starts cleanly. A multi-commit sequence keeps its remaining commits and records that the stopped commit was concluded here; finish it with `libra cherry-pick --skip` (or `--quit`) or `libra revert --abort`. A reset with pathspecs changes no sequence state, and an in-progress rebase is left alone. If that bookkeeping fails, the reset still succeeds and the leftover state is named in a warning.

The default target is `HEAD`, making `libra reset` (with no arguments) equivalent to un-staging everything.

## Options

| Flag | Long | Value | Description |
|------|------|-------|-------------|
| | `<target>` | positional (default: `HEAD`) | Commit, branch, or revision expression to reset to |
| | `--soft` | | Move HEAD only; keep index and working tree |
| | `--mixed` | | Move HEAD and reset index; keep working tree (default) |
| | `--hard` | | Move HEAD, reset index, and restore working tree |
| | `--merge` | | Reset HEAD/index, update safe paths, and preserve unstaged changes |
| | `--keep` | | Reset HEAD/index while refusing to overwrite local changes on affected paths |
| | `<pathspec>...` | positional, optionally after `--` | Specific files to reset in the index |
| | `--pathspec-from-file` | `<file>` | Read pathspecs from a file (`-` for stdin) instead of the command line. Mutually exclusive with command-line pathspecs |
| | `--pathspec-file-nul` | | Treat `--pathspec-from-file` input as NUL-separated rather than line-separated. No-op without `--pathspec-from-file` |
| | `--no-refresh` | | Accepted for Git compatibility; a no-op in Libra (see below) |

### Reading pathspecs from a file

`--pathspec-from-file=<file>` reads the pathspec list from a file (or from stdin when `<file>` is `-`), which is convenient for un-staging a large or scripted set of paths. Items are newline-separated by default (a trailing `\r` is stripped so CRLF files work, and blank lines are ignored); with `--pathspec-file-nul` they are NUL-separated instead.

Each item is taken **literally**. Unlike Git's default line mode, Libra does **not** perform C-style quoted-path decoding — a line such as `"a b.txt"` is interpreted as a path that literally contains the quote characters, not as `a b.txt`. For paths with special characters (spaces, newlines), use `--pathspec-file-nul` and emit the raw bytes. This matches Libra's existing literal handling of command-line pathspecs.

Supplying both `--pathspec-from-file` and command-line pathspecs is a usage error (`LBR-CLI-002`). Every pathspec — from either source — is normalised relative to the working directory and rejected if it escapes the repository (`../` traversal → `LBR-CLI-002`).

### Why `--no-refresh` is a no-op

In Git, a `--mixed` reset refreshes the index stat cache afterwards, and `--no-refresh` skips that step. Libra's reset never refreshes the index (it has no stat-refresh pass), so `--no-refresh` has nothing to skip — it is accepted purely so scripts can pass it, and it has no effect. There is no `--refresh` counterpart.

### Flag examples

```bash
# Un-stage everything (mixed reset to HEAD)
libra reset

# Move HEAD back one commit, keep changes staged
libra reset --soft HEAD~1

# Move HEAD back two commits, un-stage changes
libra reset HEAD~2

# Fully revert to a branch tip, discard all changes
libra reset --hard main

# Move to a target while preserving safe unstaged changes
libra reset --merge HEAD~1

# Move only if target-changed paths have no local changes
libra reset --keep HEAD~1

# Un-stage a specific file
libra reset src/lib.rs

# Un-stage a revision-like filename
libra reset -- HEAD

# Un-stage a specific file from an explicit target
libra reset HEAD -- src/lib.rs

# Un-stage multiple files
libra reset src/main.rs src/cli.rs

# Reset specific files to a prior commit
libra reset abc1234 -- path/to/file.rs

# Un-stage a batch of paths listed in a file
libra reset --pathspec-from-file=paths.txt

# Un-stage NUL-separated paths piped on stdin
printf 'a.txt\0b.txt' | libra reset --pathspec-from-file=- --pathspec-file-nul

# JSON output for agents
libra reset --json --hard HEAD~1
```

## Common Commands

```bash
libra reset HEAD~1                    # Move HEAD and reset index to the previous commit
libra reset --soft HEAD~2             # Move HEAD only, keep index and worktree
libra reset --hard main               # Reset HEAD, index, and worktree to branch 'main'
libra reset --merge HEAD~1            # Preserve safe unstaged worktree changes
libra reset --keep HEAD~1             # Refuse if affected paths have local changes
libra reset src/lib.rs                 # Unstage a path back to HEAD
libra reset HEAD -- src/lib.rs        # Unstage a path back to HEAD
libra reset --pathspec-from-file=paths.txt   # Unstage paths read from a file ('-' for stdin)
libra reset --json --hard HEAD~1      # Structured JSON output for agents
```

## Human Output

Full reset (no pathspecs):

```text
HEAD is now at abc1234 Initial commit
```

Pathspec reset (un-stage specific files):

```text
Unstaged changes after reset:
M	path/to/file
```

## Structured Output (JSON examples)

Full reset:

```json
{
  "ok": true,
  "command": "reset",
  "data": {
    "mode": "hard",
    "commit": "abc123def456789012345678901234567890abcd",
    "short_commit": "abc123d",
    "subject": "Initial commit",
    "previous_commit": "def456abc789012345678901234567890abcd1234",
    "files_unstaged": 0,
    "files_restored": 1,
    "pathspecs": []
  }
}
```

Pathspec reset:

```json
{
  "ok": true,
  "command": "reset",
  "data": {
    "mode": "mixed",
    "commit": "abc123def456789012345678901234567890abcd",
    "short_commit": "abc123d",
    "subject": "Initial commit",
    "previous_commit": null,
    "files_unstaged": 2,
    "files_restored": 0,
    "pathspecs": ["src/lib.rs", "src/cli.rs"]
  }
}
```

### Schema Notes

- When `pathspecs` is non-empty, the command performs a mixed reset on the specified paths only, without moving HEAD.
- `previous_commit` is `null` for pathspec-only resets (HEAD does not move).
- `files_restored` counts tracked files rewritten or removed by `--hard`, `--merge`, or `--keep`; a same-target clean reset can report `0`.
- `files_unstaged` counts files whose index entries were reset during mixed/pathspec resets.
- `subject` is the first line of the target commit message.

## Design Rationale

### Why reject pathspecs with whole-tree modes?

- **`--soft` + pathspecs**: `--soft` by definition only moves HEAD and touches nothing else. Resetting individual file index entries contradicts the "HEAD only" contract. If you want to un-stage specific files, use the default mixed mode: `libra reset file` or `libra reset HEAD -- file`.
- **`--hard` + pathspecs**: `--hard` restores the entire working tree to match the target commit. Selectively restoring only some files while leaving others in a different state would create a confusing hybrid that is neither "fully reset" nor "index-only reset." For selective file restoration, use `libra restore --source <commit> -- file`.
- **`--merge`/`--keep` + pathspecs**: their safety checks compare target, HEAD, index, and worktree as one transition. A partial path set would change that preservation contract; use mixed pathspec reset or `libra restore` instead.

This keeps pathspec reset an index-only mixed operation, while every working-tree mode remains a whole-tree transition.

### Why default to mixed?

Mixed mode is the safest general-purpose reset: it un-stages changes without discarding work. A developer who runs `libra reset HEAD~1` without thinking about modes gets their changes preserved in the working tree as unstaged modifications. This matches Git's default and is the least surprising behavior for the most common use case (un-staging files or amending a commit).

### `--merge` versus `--keep`

`--merge` protects unstaged index-to-worktree changes and may discard staged changes when doing so is safe. `--keep` is stricter on paths changed between the old HEAD and target: either staged or unstaged local changes make it refuse. Neither mode creates conflict markers; unsafe transitions fail before mutation with `LBR-CONFLICT-002`.

## Parameter Comparison: Libra vs Git vs jj

| Feature | Git | Libra | jj |
|---------|-----|-------|----|
| Mixed reset (default) | `git reset <target>` | `libra reset <target>` | N/A (jj has no staging area) |
| Soft reset | `git reset --soft <target>` | `libra reset --soft <target>` | N/A |
| Hard reset | `git reset --hard <target>` | `libra reset --hard <target>` | `jj restore --from <rev>` |
| Un-stage files | `git reset <file>` / `git reset HEAD -- <file>` | `libra reset <file>` / `libra reset HEAD -- <file>` | N/A (no staging area) |
| Merge reset | `git reset --merge <target>` | `libra reset --merge <target>` | N/A |
| Keep reset | `git reset --keep <target>` | `libra reset --keep <target>` | N/A |
| Pathspec from file | `git reset --pathspec-from-file=<f>` | `libra reset --pathspec-from-file=<f>` (literal paths; no C-style quote decoding) | N/A |
| Pathspec file NUL | `git reset --pathspec-file-nul` | `libra reset --pathspec-file-nul` | N/A |
| Index refresh control | `git reset --[no-]refresh` | `--no-refresh` accepted as a no-op; no `--refresh` | N/A |
| Default target | HEAD | HEAD | N/A |
| Structured output | No | `--json` / `--machine` | `--template` |
| Pathspec + soft | Rejected | Rejected (`LBR-CLI-002`) | N/A |
| Pathspec + hard | Rejected | Rejected (`LBR-CLI-002`) | N/A |
| Pathspec + merge/keep | Rejected | Rejected (`LBR-CLI-002`) | N/A |
| Pathspec from file + CLI pathspec | Rejected | Rejected (`LBR-CLI-002`) | N/A |
| Rollback on failure | No | Classic modes attempt commit-tree rollback; `--merge`/`--keep` restore exact index/worktree snapshots | N/A (operation log undo) |

## Error Handling

| Scenario | Error Code | Hint |
|----------|-----------|------|
| Not a libra repository | `LBR-REPO-001` | "run 'libra init' to create a repository in the current directory." |
| Invalid revision | `LBR-CLI-003` | "check the revision name and try again." |
| Ambiguous revision/path token | `LBR-CLI-002` | "use '--' to separate paths from revisions, like 'libra reset <revision> -- <file>' or 'libra reset -- <file>'." |
| HEAD is unborn | `LBR-REPO-003` | "create a commit first before resetting HEAD." |
| Failed to resolve HEAD | `LBR-IO-001` | "check whether the repository database is readable." |
| HEAD reference corrupt | `LBR-REPO-002` | "the HEAD reference or branch metadata may be corrupted." |
| Object load failure | `LBR-REPO-002` | "the object store may be corrupted." |
| Index load failure | `LBR-REPO-002` | "the index file may be corrupted." |
| Index save failure | `LBR-IO-002` | -- |
| HEAD update failure | `LBR-IO-002` | -- |
| Working tree read failure | `LBR-IO-001` | -- |
| Working tree restore failure | `LBR-IO-002` | -- |
| Invalid path encoding | `LBR-CLI-002` | "rename the path or invoke libra from a path representable as UTF-8." |
| `--soft` with pathspecs | `LBR-CLI-002` | "--soft only moves HEAD; use --mixed to reset index for specific paths." |
| `--hard` with pathspecs | `LBR-CLI-002` | "--hard updates the working tree; omit pathspecs or use --mixed for specific paths." |
| `--merge`/`--keep` with pathspecs | `LBR-CLI-002` | "--merge/--keep operate on the whole tree; omit pathspecs or use --mixed for specific paths." |
| Local changes would be overwritten by `--merge`/`--keep` | `LBR-CONFLICT-002` | "commit or stash the local changes, then retry the reset." |
| Pathspec not matched | `LBR-CLI-003` | "check the path and try again." |
| `--pathspec-from-file` with command-line pathspecs | `LBR-CLI-002` | "provide pathspecs either on the command line or via --pathspec-from-file, not both." |
| Pathspec escapes the working directory | `LBR-CLI-002` | "pathspecs must stay within the repository working directory." |
| Pathspec file/stdin read failure | `LBR-IO-001` | "check that the pathspec file exists and is readable." |
| Rollback failure | (primary code) | (primary hint) |
