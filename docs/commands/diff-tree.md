# `libra diff-tree`

Show the differences between two trees — a plumbing entry point that reuses the
shared [`diff`](diff.md) engine with plumbing exit codes, raw default output, and
rename defaults.

## Synopsis

```
libra diff-tree [-r] [-p] [-s] [--raw] [--name-only] [--name-status] [--exit-code] [--quiet] [-c]
                <tree-a> <tree-b> [-- <path>...]
```

## Description

`diff-tree` is equivalent to `libra diff --old <tree-a> --new <tree-b> --no-renames`:
it diffs the two tree-ish arguments (commits or tree ids). Path limiters go after
`--`. As Git plumbing, it ignores porcelain `diff.renames`; global output flags
such as `libra --json diff-tree ...` still apply.

Like Git's plumbing, the **default output is raw** (`:<old-mode> <new-mode>
<old-sha> <new-sha> <status>\t<path>`). Pass `-p` for a patch.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `<tree-a> <tree-b>` | The two tree-ish to compare. | `libra diff-tree HEAD~1 HEAD` |
| `-r` | Recurse into subtrees. Libra always recurses, so this is a no-op. | `libra diff-tree -r a b` |
| `-p`, `--patch` | Emit a patch instead of raw records. | `libra diff-tree -p a b` |
| `--raw` | Force raw records (the default). | `libra diff-tree --raw a b` |
| `--name-only` | Print changed path names only. | `libra diff-tree --name-only a b` |
| `--name-status` | Print path names with status letters. | `libra diff-tree --name-status a b` |
| `--exit-code` | Exit 1 when there are differences instead of the default 0. | `libra diff-tree --exit-code a b` |
| `--quiet` | Print nothing and exit 1 when there are differences. | `libra diff-tree --quiet a b` |
| `-s`, `--no-patch` | Suppress the diff body entirely. | `libra diff-tree -s a b` |
| `-c`, `--cc` | Combined-diff spelling; accepted (unmerged/combined rendering is a documented narrowing). | `libra diff-tree -c a b` |
| `-- <path>...` | Limit the diff to paths. | `libra diff-tree a b -- src/` |
| `--json` / `--machine` | Structured diff output (same envelope as `diff`). | `libra --json diff-tree a b` |

The remaining porcelain flag surface (`-t`/`--stdin`) is not exposed.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | No differences, or differences without `--exit-code`/`--quiet`. |
| `1` | There are differences and `--exit-code`/`--quiet` was given. |
| `128` | Not inside a repository. |
| `129` | Invalid arguments (`LBR-CLI-002`) or an unresolved revision (`LBR-CLI-003`). |

## Examples

```bash
# Raw records between a commit and its parent
libra diff-tree HEAD~1 HEAD

# Patch form
libra diff-tree -p HEAD~1 HEAD

# Changed names only
libra diff-tree --name-only HEAD~1 HEAD
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Diff two trees (raw) | `libra diff-tree a b` | `git diff-tree a b` |
| Recurse with `-r` | `libra diff-tree -r a b` | `git diff-tree -r a b` |
| Patch form | `libra diff-tree -p a b` | `git diff-tree -p a b` |

Deferred: single-commit `diff-tree <commit>` (vs its parent), `--stdin`, and
combined (`-c`/`--cc`) rendering. Use `libra diff` for richer options.
