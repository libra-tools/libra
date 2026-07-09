# `libra write-tree`

Write the current index out as a tree object and print its object id — the
plumbing companion to [`read-tree`](read-tree.md), equivalent to
`git write-tree`.

## Synopsis

```
libra write-tree [--index-file <path>]
```

## Description

`write-tree` reads `.libra/index` and constructs a **nested** Git tree object
(one tree per directory), writing every tree object to the object store and
printing the root tree's object id. File modes (regular, executable, symlink,
gitlink) are preserved, and the object format (SHA-1 / SHA-256) follows the
repository's hash kind.

An empty index produces the canonical empty tree
(`4b825dc642cb6eb9a060e54bf8d69288fbee4904` for SHA-1).

This is a read-only plumbing command: it writes tree objects but does not move
any ref or change the index or working tree.

Before writing any tree object, `write-tree` validates each stage-0 index entry
whose mode points at an object in this repository. Regular files, executable
files, and symlinks must point at loadable blob objects; any tree-mode entry
must point at a loadable tree object. Missing or mistyped objects fail closed
with `LBR-REPO-002`. Gitlinks (`160000`) are not validated because they refer to
submodule commits that may live outside this object database.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `--index-file <path>` | Read a scratch index instead of `.libra/index`; a missing scratch index is treated as empty. | `libra write-tree --index-file scratch.idx` |
| `--json` / `--machine` | Structured output: `{ tree: "<id>" }`. | `libra --json write-tree` |

Git's `--prefix=<prefix>` and `--missing-ok` are not exposed (deferred).

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | The tree was written; its id is printed. |
| `128` | Not inside a repository, the index/tree could not be processed, or an index object is missing/wrong-type (`LBR-REPO-002`). |

## Examples

```bash
# Write the index and capture the tree id
TREE=$(libra write-tree)

# Structured output for agents
libra --json write-tree

# Build from a scratch index
libra update-index --index-file scratch.idx --cacheinfo 100644,$OID,path/file
libra write-tree --index-file scratch.idx
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Write the index as a tree | `libra write-tree` | `git write-tree` |
| Read a tree into the index | `libra read-tree <tree>` | `git read-tree <tree>` |
