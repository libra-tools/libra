# `libra update-index`

Modify the index directly — a focused subset of `git update-index`. The
companion to [`write-tree`](write-tree.md): `--cacheinfo` registers an index
entry from an object id without reading the working tree, so an index can be
built purely from objects.

## Synopsis

```
libra update-index --add <path>...
libra update-index --remove <path>...
libra update-index --cacheinfo <mode>,<object>,<path>...
```

## Description

`update-index` applies, in order: every `--cacheinfo` entry, then the positional
paths (removed with `--remove`, otherwise (re)staged from the working tree), and
saves the index.

- `--cacheinfo <mode>,<object>,<path>` inserts/updates an entry directly. The
  object **need not exist yet** (matching Git), so you can build an index from
  hashes computed with `hash-object`. `<mode>` is an octal file mode:
  `100644` (file), `100755` (executable), `120000` (symlink), `160000`
  (gitlink). The object id length must match the repository hash format. The
  path is an index key — absolute paths and `..` traversal are rejected. A later
  `write-tree` or `commit` validates object existence/type and fails with
  `LBR-REPO-002` if a blob/tree entry still points at a missing or wrong-type
  object.
- `--add <path>...` (re)stages files from the working tree, allowing paths not
  yet tracked. Working-tree symlinks are staged as mode `120000` blobs whose
  content is the link target bytes; the target is not followed. Without
  `--add`, a positional path must already be tracked.
- `--remove <path>...` drops the named paths from the index.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `--add` | Allow positional paths to add new (untracked) files. | `libra update-index --add a.txt` |
| `--remove` | Remove the positional paths from the index. | `libra update-index --remove old.txt` |
| `--cacheinfo <mode>,<object>,<path>` | Register an entry from an object id (repeatable). | `libra update-index --cacheinfo 100644,<oid>,dir/f.txt` |
| `--json` / `--machine` | Structured output: `{ updated: <n>, removed: <n> }`. | `libra --json update-index --add a.txt` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | The index was updated and saved. |
| `128` | Not inside a repository, a usage error (bad `--cacheinfo`, untracked path without `--add`), or a missing working-tree file. |

## Examples

```bash
# Build an index entry from an object id, then write the tree
OID=$(libra hash-object -w data.bin)
libra update-index --cacheinfo 100644,"$OID",assets/data.bin
libra write-tree

# A missing cacheinfo object may be registered, but write-tree will reject it
libra update-index --cacheinfo 100644,1111111111111111111111111111111111111111,missing.bin
libra write-tree   # fails with LBR-REPO-002

# Stage and unstage working-tree files
libra update-index --add src/new.rs
libra update-index --remove src/old.rs

# Stage a working-tree symlink as a 120000 link-target blob
libra update-index --add link-to-target
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Stage a file | `libra update-index --add f` | `git update-index --add f` |
| Remove a path | `libra update-index --remove f` | `git update-index --remove f` |
| Register by id | `libra update-index --cacheinfo m,oid,p` | `git update-index --cacheinfo m,oid,p` |

Deferred (not exposed): bare-path stat refresh, `--force-remove`, `--chmod`,
`--assume-unchanged`, `--skip-worktree`, `--index-info`, and other Git flags.
