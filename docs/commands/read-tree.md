# `libra read-tree`

Read a tree object into the index — the plumbing companion to
[`write-tree`](write-tree.md), a focused subset of `git read-tree`.

## Synopsis

```
libra read-tree <tree-ish>
```

## Description

`read-tree` resolves `<tree-ish>` to a tree, flattens it into stage-0 index
entries, and **replaces** `.libra/index` with that content. `<tree-ish>` may be:

- a tree object id,
- a commit object id (peeled to its tree),
- a ref, tag, branch name, or `HEAD` (peeled to its tree).

This first version is **index-only**: it never touches the working tree, so it
cannot silently overwrite working-tree files. The Git options that would modify
the working tree or perform a merge (`-u`, `-m`, `--reset`, `--prefix`) are not
exposed — use `libra restore` / `libra checkout` to update the working tree.

`-m`/`--merge` merges the tree into the current index: entries from the tree are added or updated, entries the tree does not mention are kept, and replaced entries keep their index v3 extended flags (skip-worktree / intent-to-add). Without `-m` the index is rebuilt from the tree and the extended flags are cleared, matching Git's rebuild semantics.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `<tree-ish>` | The tree to read (tree id, commit, ref, tag, or `HEAD`). | `libra read-tree HEAD` |
| `--json` / `--machine` | Structured output: `{ tree: "<id>", entries: <n> }`. | `libra --json read-tree HEAD` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | The tree was read into the index. |
| `128` | Not inside a repository, or `<tree-ish>` is not a valid tree-ish. |

## Examples

```bash
# Reset the index to HEAD's tree (working tree untouched)
libra read-tree HEAD

# Read a specific tree id captured from write-tree
TREE=$(libra write-tree)
libra read-tree "$TREE"

# Structured output for agents
libra --json read-tree HEAD
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Read a tree into the index | `libra read-tree <tree>` | `git read-tree <tree>` |
| Write the index as a tree | `libra write-tree` | `git write-tree` |

Deferred (not exposed): `-m` (merge), `-u` (update working tree), `--reset`,
`--prefix`, multi-tree merges.
