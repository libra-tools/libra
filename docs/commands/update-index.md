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
paths, and saves the index. `--add` and `--remove` are permissions rather than
alternatives: `--remove` alone drops each named path from the index, `--add` alone
(re)stages it from the working tree, and given together they route per path by
whether it still exists — an existing path is staged, a missing one is dropped.

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
- `--remove <path>...` drops the named paths from the index. Given together with
  `--add`, presence on disk decides per path: an existing path is staged, a missing
  one is dropped — which is how upstream setup steps use `--add --remove` in one
  call. `--remove` on its own always drops the path, whether or not it still exists.

Working-tree staging returns `LBR-IO-002` if the blob or its durable cloud
index marker cannot be written; it does not panic or save an index entry that
lacks repair ownership. The failure message follows the canonical wording:
the object payloads were stored safely, no paths were staged, and a direct
retry reuses the already-stored payloads without any lock-file cleanup (lock
timeouts additionally name the lock holder, and lock files must never be
deleted). A normal retry re-registers a payload that the failed attempt
already persisted.

Local index persistence and the later cloud-catalog update have separate
durability boundaries. If a terminal background `object_index` error happens
after the blob and index are saved, `update-index` keeps its normal success
output, emits an actionable stderr warning, and retains an atomic repair marker
for the next schema-aware repository command. `cloud sync` and destructive
agent cleanup fail closed while repair remains pending. With
`--exit-code-on-warning`, the completed local update returns exit 9 /
`LBR-WARN-001`; retrying `update-index` is unnecessary.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `--add` | Allow positional paths to add new (untracked) files. | `libra update-index --add a.txt` |
| `--remove` | Remove the positional paths from the index. With `--add`, presence on disk decides per path (existing → staged, missing → removed). | `libra update-index --remove old.txt` |
| `--force-remove` | Drop the positional paths from the index regardless of whether the working-tree file exists; paths the index does not know are a no-op, and every stage of an unmerged path is removed. Wins over `--add`/`--remove`. | `libra update-index --force-remove old.txt` |
| `--add --remove` | Both permissions at once: stage what exists, drop what is gone. | `libra update-index --add --remove a.txt gone.txt` |
| `--cacheinfo <mode>,<object>,<path>` | Register an entry from an object id (repeatable). | `libra update-index --cacheinfo 100644,<oid>,dir/f.txt` |
| `--json` / `--machine` | Structured output: `{ updated: <n>, removed: <n> }`. | `libra --json update-index --add a.txt` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | The index was updated and saved. |
| `9` / `LBR-WARN-001` | The local index was saved, but cloud index repair remains pending and `--exit-code-on-warning` was used. |
| `128` | Not inside a repository, a usage error (bad `--cacheinfo`, untracked path without `--add`), or a missing working-tree file. |
| `128` / `LBR-IO-002` | Working-tree blob or durable cloud index-marker persistence failed; fix storage permissions and retry. |

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
| Remove a path even when the file is gone | `libra update-index --force-remove f` | `git update-index --force-remove f` |
| Register by id | `libra update-index --cacheinfo m,oid,p` | `git update-index --cacheinfo m,oid,p` |

Deferred (not exposed): bare-path stat refresh, `--chmod`,
`--assume-unchanged`, `--skip-worktree`, `--index-info`, and other Git flags.
