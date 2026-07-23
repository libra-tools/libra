# `libra merge-file`

Run a file-level three-way merge — a focused subset of `git merge-file`. Merges
`<current>` and `<other>` relative to their common ancestor `<base>`, using the
same `diffy` three-way merge that `libra merge` uses for blob contents, so the
conflict markers are identical.

## Synopsis

```
libra merge-file [-p|--stdout] [--diff3] [-q|--quiet] <current> <base> <other>
```

## Description

`merge-file` incorporates the changes that lead from `<base>` to `<other>` into
`<current>`. Where both sides changed the same lines, a conflict is recorded
with markers:

```
<<<<<<< ours
...lines from <current>...
=======
...lines from <other>...
>>>>>>> theirs
```

With `--diff3`, the base section is included between `|||||||` and `=======`.

By default the result is written back into `<current>`; with `-p` it is printed
to stdout and no file is touched. When writing in place inside a repository, the
original `<current>` is first copied to this worktree's `merge-file-backup/` (in its local gitdir — each worktree keeps its own backups); the backup
is removed on a clean merge and kept (with a note) when conflicts remain.

The three arguments are read as raw bytes; `merge-file` does not require them to
be tracked or to correspond to stored blobs.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `-p`, `--stdout` | Print the merged result to stdout; do not modify `<current>`. | `libra merge-file -p a b c` |
| `--diff3` | Include the `<base>` section in conflict markers. | `libra merge-file --diff3 -p a b c` |
| `-q`, `--quiet` | Do not warn about conflicts on stderr. | `libra merge-file -q a b c` |
| `--json` / `--machine` | Structured output: `{ conflict, written, merged? }` (`merged` only with `-p`). | `libra --json merge-file -p a b c` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Clean merge (no conflicts). |
| `1` | The merge produced conflicts (markers are still emitted). Fixed at `1` regardless of the number of conflicts. |
| `128` | Error: a missing/unreadable input, or a binary file (a NUL byte was found). |

## Examples

```bash
# Print a merged result without touching any file
libra merge-file -p ours.txt base.txt theirs.txt

# Merge in place into ours.txt (backed up under this worktree's merge-file-backup/)
libra merge-file ours.txt base.txt theirs.txt

# diff3-style markers that also show the common ancestor
libra merge-file --diff3 -p ours.txt base.txt theirs.txt
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Three-way merge to stdout | `libra merge-file -p a b c` | `git merge-file -p a b c` |
| Merge in place | `libra merge-file a b c` | `git merge-file a b c` |
| diff3 markers | `libra merge-file --diff3 …` | `git merge-file --diff3 …` |

Differences and deferred options: conflict markers are labelled `ours` / `theirs`
(consistent with `libra merge`), not the file names; the conflict exit code is
fixed at `1` (Git reports the conflict count); and `-L <label>`, `--ours` /
`--theirs` / `--union`, and `--marker-size` are not yet exposed.
