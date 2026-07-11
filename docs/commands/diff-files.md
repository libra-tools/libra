# `libra diff-files`

Show the differences between the index and the working tree — a plumbing entry
point that reuses the one `diff` engine (see [`diff`](diff.md)).

## Synopsis

```
libra diff-files [-- <path>...]
```

## Description

`diff-files` is equivalent to `libra diff --no-renames`: it shows the unstaged
changes (index vs working tree). Path limiters go after `--`; as Git plumbing it
ignores porcelain `diff.renames`. Global output flags such as
`libra --json diff-files` still apply.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `-- <path>...` | Limit the diff to paths. | `libra diff-files -- src/` |
| `--json` / `--machine` | Structured diff output. | `libra --json diff-files` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | No differences. |
| `1` | There are differences (the diff is printed) — Git plumbing exit convention. |
| `128` | Not inside a repository. |

## Examples

```bash
# Show unstaged changes
libra diff-files

# Limit to a directory
libra diff-files -- src/
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Unstaged changes | `libra diff-files` | `git diff-files` |

Deferred: `-1`/`-2`/`-3` stage selection and raw output. Use `libra diff` for
richer options.
