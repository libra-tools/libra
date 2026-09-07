# `libra merge-base`

Find the best common ancestor(s) of two commits — a focused subset of
`git merge-base`. Backed by the single lowest-common-ancestor (LCA)
implementation in `internal/merge_base.rs`, which `diff A...B` also uses.

## Synopsis

```
libra merge-base <commit> <commit>
libra merge-base --all <commit> <commit>
libra merge-base --is-ancestor <commit> <commit>
```

## Description

Given two commits, `merge-base` prints their best common ancestor — a true LCA:
a common ancestor that is not itself a strict ancestor of another common
ancestor. For a normal "Y" history that is the point where the branches
diverged. In criss-cross histories there can be several LCAs; `--all` prints all
of them, while the default prints one (deterministically chosen).

With `--is-ancestor`, nothing is printed; the exit code answers whether the
first commit is an ancestor of the second.

Each `<commit>` may be a branch, tag, `HEAD`, or an object id.

The search paints down from both commits at once over a committer-date priority
queue (the same shape as Git's `paint_down_to_common`): one combined traversal
carrying both sides' marks, instead of a separate full walk per side followed by
another per common ancestor. A commit can be visited again as it picks up the
second side's mark, so "one traversal" means one pass over the graph, not one
visit per commit. Results are unchanged — committer dates only decide visit
order, so a history with skewed or identical dates yields the same bases.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `--all` | Print every lowest common ancestor, not just one. | `libra merge-base --all main feature` |
| `--is-ancestor` | Test ancestry (exit 0/1) instead of printing a base. | `libra merge-base --is-ancestor v1 main` |
| `--json` / `--machine` | Structured output: `{ bases: [...] }` or `{ is_ancestor }`. | `libra --json merge-base main feature` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | A merge base was printed, or (`--is-ancestor`) the first commit is an ancestor of the second. |
| `1` | No common ancestor exists, or (`--is-ancestor`) the first commit is not an ancestor of the second. No output. |
| `128` | A commit could not be resolved, or the wrong number of arguments was given. |

## Examples

```bash
# Where did main and feature diverge?
libra merge-base main feature

# Is the release tag still on the main line?
libra merge-base --is-ancestor v1.0 main && echo "yes, fast-forwardable"

# Diff a feature against where it branched from main
libra diff main...feature
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Best common ancestor | `libra merge-base a b` | `git merge-base a b` |
| All merge bases | `libra merge-base --all a b` | `git merge-base --all a b` |
| Ancestry test | `libra merge-base --is-ancestor a b` | `git merge-base --is-ancestor a b` |

Deferred (not yet exposed): more than two commits and `--octopus` /
`--independent` / `--fork-point`. `merge`, `rebase`, `am` and `diff A...B` all
compute through this shared LCA; only `log A...B` still has its own reachable-set
implementation, and migrating it is a tracked follow-up.
