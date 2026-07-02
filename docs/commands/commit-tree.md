# `libra commit-tree`

Plumbing: create a commit object from an existing tree — no index, worktree,
HEAD, or ref side effects (lore.md §1.15).

## Synopsis

```
libra commit-tree <tree> [-p <parent>]... [-m <paragraph>]... [-F <file>]...
```

## Description

Wraps a tree + parents + message into a commit object, writes it to the
object store, and prints the OID. Nothing else changes: publish the result
explicitly with `libra update-ref` (whose protect/archive policy guards
`refs/heads/*`). Together with the `--index-file` scratch flag on
`update-index` / `write-tree` / `read-tree`, this closes the Git-idiomatic
off-worktree revision-composition loop:

```bash
BLOB=$(libra hash-object -w --stdin < content)
libra update-index --index-file scratch.idx --add --cacheinfo "100644,$BLOB,path/file"
TREE=$(libra write-tree --index-file scratch.idx)
COMMIT=$(libra commit-tree "$TREE" -p HEAD -m "composed")
libra update-ref refs/heads/topic "$COMMIT"
```

`<tree>` accepts a tree OID; commit-ish/refs/tags peel to their tree (a
documented Libra superset of Git). `-p` is repeatable (order preserved;
duplicates warn and are ignored, like Git); parents must load as commits.
The message comes from repeatable `-m` paragraphs, `-F` files (`-` =
stdin), or bare piped stdin; `-m` and `-F` may be combined (all `-m`
paragraphs first, then `-F` — argv interleaving is not preserved,
documented). Messages pass through byte-exact (trailer blocks from
1.9/1.10 hash verbatim). `--json` emits `{"commit": "<oid>"}`.

## Intentional differences from Git

- Empty messages are refused (the repo-wide rule; git plumbing accepts them)
  — replaying foreign history with empty messages is not yet possible.
- Commits are always unsigned in v1 (git honors `commit.gpgsign` here);
  vault signing is a recorded follow-up.
- `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` are not honored yet, so OIDs are
  not reproducible across runs (recorded follow-up).
- A TTY with no `-m`/`-F` is a usage error rather than an interactive wait
  (agent-safe).

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Commit object written; OID printed. |
| `128` | Fatal (unresolvable tree/parent, not a repository, write failure). |
| `129` | Usage (no/empty message on a TTY, invalid arguments). |

## Examples

```bash
libra commit-tree $TREE -m 'root commit'
libra commit-tree $TREE -p HEAD -m subject -m 'Reviewed-by: Alice <a@e>'
echo msg | libra commit-tree $TREE -p A -p B
libra commit-tree HEAD -m 'same tree, new message'
```
