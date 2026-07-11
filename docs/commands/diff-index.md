# `libra diff-index`

Show the differences between a tree and the working tree — a plumbing entry
point that reuses the one `diff` engine (see [`diff`](diff.md)).

## Synopsis

```
libra diff-index <tree> [-- <path>...]
```

## Description

`diff-index <tree>` is equivalent to `libra diff --old <tree> --no-renames`: it
diffs the given tree-ish against the current working tree. Path limiters go
after `--`; as Git plumbing it ignores porcelain `diff.renames`. Global output
flags such as `libra --json diff-index ...` still apply.

`--cached` (compare the tree against the index) is **not yet supported**; use
`libra diff --staged` for HEAD vs the index.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `<tree>` | The tree-ish to compare against the working tree. | `libra diff-index HEAD` |
| `--cached` / `--cached` | Compare against the index (not yet supported → exit 128). | |
| `-- <path>...` | Limit the diff to paths. | `libra diff-index HEAD -- src/` |
| `--json` / `--machine` | Structured diff output. | `libra --json diff-index HEAD` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | No differences. |
| `1` | There are differences (the diff is printed) — Git plumbing exit convention. |
| `128` | The tree could not be resolved, `--cached` was given (unsupported), or not inside a repository. |

## Examples

```bash
# What has changed in the working tree relative to HEAD's tree?
libra diff-index HEAD

# HEAD vs the index (use diff --staged for now)
libra diff --staged
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Tree vs working tree | `libra diff-index <tree>` | `git diff-index <tree>` |
| Tree vs index | `libra diff --staged` (HEAD only) | `git diff-index --cached <tree>` |

Deferred: `--cached` against an arbitrary tree, `--stat`-only raw output, and
`-m`. Use `libra diff` for richer options.
