# `libra diff-files`

Show the differences between the index and the working tree — a plumbing entry
point that reuses the one `diff` engine (see [`diff`](diff.md)).

## Synopsis

```
libra diff-files [-p] [-s] [--raw] [--name-only] [--name-status] [--exit-code] [--quiet] [-- <path>...]
```

## Description

`diff-files` compares the index with the working tree through the shared diff
engine, retaining Git plumbing stat-based candidate selection. Path limiters go after `--`; as Git plumbing it
ignores porcelain `diff.renames`. Global output flags such as
`libra --json diff-files` still apply.

Like Git's plumbing, the **default output is raw**. `--name-status` prints Git's
`A`/`M`/`D`/`R` letters (for example `A\tnew.txt` for an intent-to-add entry).
The worktree side is **stat-based**: a stat-dirty entry whose content is
unchanged is reported `M` with an all-zero worktree sha, without hashing — the
same contract behind `git diff-files` and `t3700-add.sh#24`. The status letter
follows entry presence, not content emptiness: an empty tracked blob whose stat
was invalidated (for example by `read-tree`) is `M`, not `A`, and a worktree
file truncated to zero bytes is `M`, not `D`.

With explicit `text`, `eol=lf`, or `eol=crlf`, `-p` compares clean worktree
content (CRLF pairs become LF). Its patch may therefore be empty even when
stat-based raw/name output still reports `M` with a zero worktree OID.
`--quiet` and `--exit-code` retain that stat-based difference result, including
exit `1` with an empty patch. Use [`diff`](diff.md) for content-based clean
checks. `-text` preserves bytes; `text=auto` also converts text input while protecting binary and historical CRLF index content; see [add](add.md) for limits. Stored objects
are not converted, and the command does not refresh the index or rewrite
worktree files. A failure while reading content or attributes for a patch is
an error, not an empty successful patch.

In legacy symlink worktrees, content comparisons read `info/attributes` from the resolved metadata directory. Attribute files must be readable regular files; symlink attribute files are rejected. Raw output and difference exit codes keep the stat-based behavior described above.


Plumbing raw records use full-width object ids (40-hex SHA-1 / 64-hex SHA-256) and a full-width all-zero id for the missing side (e.g. the worktree side of `diff-index`/`diff-files`). `-p --raw` prints the raw records, a blank line, then the patch, matching Git.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `-p`, `--patch` | Emit a patch instead of raw records. | `libra diff-files -p` |
| `--raw` | Force raw records (the default). | `libra diff-files --raw` |
| `--name-only` | Print changed path names only. | `libra diff-files --name-only` |
| `--name-status` | Print the status letter for each changed path. | `libra diff-files --name-status` |
| `-s`, `--no-patch` | Suppress the diff body entirely. | `libra diff-files -s` |
| `--exit-code` | Exit 1 when there are differences instead of the default 0. | `libra diff-files --exit-code` |
| `--quiet` | Print nothing and exit 1 when there are differences. | `libra diff-files --quiet` |
| `-- <path>...` | Limit the diff to paths. | `libra diff-files -- src/` |
| `--json` / `--machine` | Structured diff output. | `libra --json diff-files` |

The remaining porcelain flag surface (`-1`/`-2`/`-3` stage selection) is not exposed.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | No differences, or differences without `--exit-code`/`--quiet` (Git plumbing convention). |
| `1` | There are differences and `--exit-code`/`--quiet` was given. |
| `128` | Not inside a repository, or content/attribute reading failed. |
| `129` | Invalid arguments (`LBR-CLI-002`). |

## Examples

```bash
# Raw unstaged records
libra diff-files

# Patch form
libra diff-files -p

# Names only
libra diff-files --name-only
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Unstaged changes (raw) | `libra diff-files` | `git diff-files` |
| Patch form | `libra diff-files -p` | `git diff-files -p` |

Deferred: `-1`/`-2`/`-3` stage selection. Use `libra diff` for richer options.
