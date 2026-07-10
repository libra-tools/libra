# `libra reset`

Move `HEAD` and reset the index or working tree depending on the selected mode.

## Synopsis

```
libra reset [<target>] [--soft | --mixed | --hard]
libra reset <pathspec>...
libra reset [<target>] [--] <pathspec>...
libra reset [<target>] --pathspec-from-file=<file> [--pathspec-file-nul]
```

## Description

`libra reset` moves the HEAD reference to a target commit and optionally resets the index and working tree to match. The three modes control how much state is affected:

- **`--soft`**: moves HEAD only. The index and working tree are untouched, so all differences between the old HEAD and the target appear as staged changes. Useful for squashing commits.
- **`--mixed`** (default): moves HEAD and resets the index. The working tree is untouched, so changes appear as unstaged modifications. Useful for un-staging files.
- **`--hard`**: moves HEAD, resets the index, and restores the working tree. All uncommitted changes are discarded. Useful for fully reverting to a known state.

When `--hard` restores a tree entry whose mode is `120000`, Libra creates a
real symlink on Unix from the stored link target bytes. If a regular file
currently occupies that path, it is replaced by the symlink; if a symlink
currently occupies a path that should become a regular file, the link itself is
removed before writing. Platforms without symlink support report an explicit
unsupported diagnostic.

When pathspecs are provided, the command performs a targeted mixed reset: only the named files are reset in the index to match the target commit, without moving HEAD. This is the primary way to un-stage specific files. Like Git, a bare first positional that is a known path and not a revision is treated as a pathspec with target `HEAD`, so `libra reset src/lib.rs` is equivalent to `libra reset HEAD -- src/lib.rs`. If a token is both a revision and a filename, reset refuses it as ambiguous; use `libra reset <revision> -- <file>` for a target revision or `libra reset -- <file>` for a path. Pathspecs are incompatible with `--soft` and `--hard`. When a pathspec reset restores a symlink from the target commit, the index entry keeps mode `120000` and the blob remains the link target bytes.

The default target is `HEAD`, making `libra reset` (with no arguments) equivalent to un-staging everything.

## Options

| Flag | Long | Value | Description |
|------|------|-------|-------------|
| | `<target>` | positional (default: `HEAD`) | Commit, branch, or revision expression to reset to |
| | `--soft` | | Move HEAD only; keep index and working tree |
| | `--mixed` | | Move HEAD and reset index; keep working tree (default) |
| | `--hard` | | Move HEAD, reset index, and restore working tree |
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
- `files_restored` counts tracked files rewritten or removed by `--hard`; on a clean repository, `reset --hard HEAD` can report `0`.
- `files_unstaged` counts files whose index entries were reset during mixed/pathspec resets.
- `subject` is the first line of the target commit message.

## Design Rationale

### Why reject pathspecs with --soft/--hard?

- **`--soft` + pathspecs**: `--soft` by definition only moves HEAD and touches nothing else. Resetting individual file index entries contradicts the "HEAD only" contract. If you want to un-stage specific files, use the default mixed mode: `libra reset file` or `libra reset HEAD -- file`.
- **`--hard` + pathspecs**: `--hard` restores the entire working tree to match the target commit. Selectively restoring only some files while leaving others in a different state would create a confusing hybrid that is neither "fully reset" nor "index-only reset." For selective file restoration, use `libra restore --source <commit> -- file`.

This restriction makes the three modes unambiguous: soft touches HEAD, mixed touches HEAD + index, hard touches HEAD + index + worktree. Pathspecs operate orthogonally at the index level only.

### Why default to mixed?

Mixed mode is the safest general-purpose reset: it un-stages changes without discarding work. A developer who runs `libra reset HEAD~1` without thinking about modes gets their changes preserved in the working tree as unstaged modifications. This matches Git's default and is the least surprising behavior for the most common use case (un-staging files or amending a commit).

### Why no --merge/--keep?

Git's `--merge` and `--keep` modes attempt to preserve uncommitted changes during a reset by performing a three-way merge between the old HEAD, the new HEAD, and the working tree. These modes are:

- **Rarely used**: most developers use `--soft`, `--mixed`, or `--hard` exclusively. The merge/keep modes add complexity for a niche use case.
- **Difficult to reason about**: the three-way merge during reset can produce conflicts, leaving the repository in a state that is neither "reset" nor "unchanged." This is confusing for both humans and AI agents.
- **Replaceable by explicit workflows**: the same result is achievable with `libra stash && libra reset --hard <target> && libra stash pop`, which makes each step visible and debuggable.

Libra favors explicit, composable commands over implicit multi-step operations hidden behind a single flag.

## Parameter Comparison: Libra vs Git vs jj

| Feature | Git | Libra | jj |
|---------|-----|-------|----|
| Mixed reset (default) | `git reset <target>` | `libra reset <target>` | N/A (jj has no staging area) |
| Soft reset | `git reset --soft <target>` | `libra reset --soft <target>` | N/A |
| Hard reset | `git reset --hard <target>` | `libra reset --hard <target>` | `jj restore --from <rev>` |
| Un-stage files | `git reset <file>` / `git reset HEAD -- <file>` | `libra reset <file>` / `libra reset HEAD -- <file>` | N/A (no staging area) |
| Merge reset | `git reset --merge <target>` | Not supported | N/A |
| Keep reset | `git reset --keep <target>` | Not supported | N/A |
| Pathspec from file | `git reset --pathspec-from-file=<f>` | `libra reset --pathspec-from-file=<f>` (literal paths; no C-style quote decoding) | N/A |
| Pathspec file NUL | `git reset --pathspec-file-nul` | `libra reset --pathspec-file-nul` | N/A |
| Index refresh control | `git reset --[no-]refresh` | `--no-refresh` accepted as a no-op; no `--refresh` | N/A |
| Default target | HEAD | HEAD | N/A |
| Structured output | No | `--json` / `--machine` | `--template` |
| Pathspec + soft | Rejected | Rejected (`LBR-CLI-002`) | N/A |
| Pathspec + hard | Rejected | Rejected (`LBR-CLI-002`) | N/A |
| Pathspec from file + CLI pathspec | Rejected | Rejected (`LBR-CLI-002`) | N/A |
| Rollback on failure | No | Attempts index rollback | N/A (operation log undo) |

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
| Pathspec not matched | `LBR-CLI-003` | "check the path and try again." |
| `--pathspec-from-file` with command-line pathspecs | `LBR-CLI-002` | "provide pathspecs either on the command line or via --pathspec-from-file, not both." |
| Pathspec escapes the working directory | `LBR-CLI-002` | "pathspecs must stay within the repository working directory." |
| Pathspec file/stdin read failure | `LBR-IO-001` | "check that the pathspec file exists and is readable." |
| Rollback failure | (primary code) | (primary hint) |
