# `libra ls-tree`

List the contents of a tree object.

## Synopsis

```bash
libra ls-tree [OPTIONS] <TREE-ISH> [PATH...]
```

Without -r or -d, a single directory path without a trailing slash lists the directory entry itself; with a trailing slash it lists the directory contents.

A final `/.` also selects contents. `-t` includes trees traversed to reach a path; `-r` recurses into selected directories. Overlapping arguments expand a directory when another argument traverses it, and duplicate entries are emitted once.

Compatibility limits: multiple filters retain argument order rather than Git tree order. Full pathspec magic, subdirectory `-t` parent display, missing-path and blob trailing-slash behavior, and `-d` with trailing-slash directory filters are not fully Git-compatible.

## Description

`libra ls-tree` resolves `<TREE-ISH>` to a commit root tree or to a tree object
hash, then prints entries from that tree. When invoked from a subdirectory,
paths and output are relative to that subdirectory by default. It is read-only:
it does not update refs, the index, the worktree, or object storage.

The current compatibility slice supports ordinary path prefix filters,
`--full-name`, `--full-tree`, `REV:path` tree-ish syntax (resolve a revision
and navigate into a subtree, e.g. `HEAD:src`), and `--format`. Full Git
pathspec magic is deferred.

## Options

| Flag | Description |
|------|-------------|
| `-r`, `--recursive` | Recurse into subtrees |
| `-t` | Show tree entries while recursing, and the trees traversed to reach each path argument |
| `-d` | Show matching tree entries themselves rather than their children |
| `-l`, `--long` | Show blob sizes; tree and commit entries use `-` |
| `-z` | Terminate records with NUL instead of newline |
| `--name-only` | Print only entry paths |
| `--name-status` | Git-compatible alias that prints only entry paths |
| `--object-only` | Print only object IDs |
| `--full-name` | Print paths relative to the repository root when invoked from a subdirectory |
| `--full-tree` | List from the repository root and interpret path filters relative to the repository root |
| `--abbrev[=<N>]` | Abbreviate object IDs to `N` characters, or 7 when `N` is omitted |
| `<TREE-ISH>` | Commit, branch, tag, `HEAD`, or tree object hash |
| `[PATH...]` | Optional path filters; a directory path without a trailing slash selects the directory entry itself (its contents when another path argument names a path inside it), and with a trailing slash its contents; relative to the current directory unless `--full-tree` is set |

## Examples

```bash
libra ls-tree HEAD
libra ls-tree HEAD:src
libra ls-tree HEAD:src/nested
libra ls-tree -r HEAD src
libra ls-tree -l HEAD README.md
libra ls-tree --name-only HEAD src/
libra ls-tree --full-name HEAD
libra ls-tree --full-tree HEAD
libra ls-tree --object-only --abbrev HEAD
libra ls-tree -z HEAD
libra --json ls-tree HEAD
```

## Human Output

Default output matches Git's common shape:

```text
100644 blob 4f3c2d1a7b8c9d0e1234567890abcdef12345678	README.md
040000 tree 5a6b7c8d9e0f1234567890abcdef1234567890	src
```

With `-l`, blob entries include their decoded object size:

```text
100644 blob 4f3c2d1a7b8c9d0e1234567890abcdef12345678      128	README.md
040000 tree 5a6b7c8d9e0f1234567890abcdef1234567890        -	src
```

## Structured Output

With `--json`, output uses the standard command envelope. With `-l`, blob
entries include the `size` field:

```json
{
  "ok": true,
  "command": "ls-tree",
  "data": {
    "treeish": "HEAD",
    "root_tree": "5a6b7c8d9e0f1234567890abcdef1234567890",
    "recursive": false,
    "entries": [
      {
        "mode": "100644",
        "object_type": "blob",
        "object": "4f3c2d1a7b8c9d0e1234567890abcdef12345678",
        "path": "README.md",
        "size": 128
      }
    ]
  }
}
```

## Parameter Comparison: Libra vs Git vs jj

| Feature | Libra | Git | jj |
|---------|-------|-----|----|
| Commit/tree listing | Supported | Supported | Use file/revset commands |
| Recursive listing | `-r` / `--recursive` | `-r` | Different model |
| Tree entries while recursing, and traversed parent trees | `-t` | `-t` | Different model |
| Subdirectory output | Current-directory relative; `--full-name` keeps repository paths | Supported | Different model |
| Root-scoped listing | `--full-tree` | `--full-tree` | Different model |
| Path filters | Prefix filters only; current-directory relative unless `--full-tree` is set | Full pathspec | Revset/file patterns |
| Custom formatting | Deferred | `--format` | Different model |
| JSON output | `--json` | No | No |

## Error Handling

| Scenario | StableErrorCode | Exit |
|----------|-----------------|------|
| Invalid or missing tree-ish | `LBR-CLI-003` | 129 |
| `REV:path` targets a blob (not a tree) | `LBR-CLI-003` | 128 |
| Failed to read objects | `LBR-IO-001` | 128 |
| Corrupt stored refs/objects | `LBR-REPO-002` | 128 |
