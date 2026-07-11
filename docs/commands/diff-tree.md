# `libra diff-tree`

Show the differences between two trees — a plumbing entry point that reuses the
shared [`diff`](diff.md) engine with plumbing exit codes and rename defaults.

## Synopsis

```
libra diff-tree <tree-a> <tree-b> [-- <path>...]
```

## Description

`diff-tree` is equivalent to `libra diff --old <tree-a> --new <tree-b> --no-renames`: it
diffs the two tree-ish arguments (commits or tree ids). Path limiters go after
`--`. As Git plumbing, it ignores porcelain `diff.renames`; global output flags
such as `libra --json diff-tree ...` still apply.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `<tree-a> <tree-b>` | The two tree-ish to compare. | `libra diff-tree HEAD~1 HEAD` |
| `-- <path>...` | Limit the diff to paths. | `libra diff-tree a b -- src/` |
| `--json` / `--machine` | Structured diff output (same envelope as `diff`). | `libra --json diff-tree a b` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | No differences. |
| `1` | There are differences (the diff is printed) — Git plumbing exit convention. |
| `128` | A tree-ish could not be resolved, or not inside a repository. |

## Examples

```bash
# Diff between a commit and its parent
libra diff-tree HEAD~1 HEAD

# Limit to a directory
libra diff-tree main feature -- src/
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Diff two trees | `libra diff-tree a b` | `git diff-tree a b` |

Deferred: single-commit `diff-tree <commit>` (vs its parent), `-r`/`-t`/`--stdin`,
and raw output format. Use `libra diff` for richer options.
