# `libra op`

Inspect and restore command-level operation history.

## Synopsis

```bash
libra op log [OPTIONS]
libra op show [OPTIONS] <OP_REF>
libra op restore [OPTIONS] <OP_REF>
```

## Description

`libra op` provides a command-line surface over the operation graph persisted by
the operation service and wrapper layers.

It currently supports three subcommands:

- `op log`: list recorded operations with pagination and optional command filter.
- `op show`: inspect one operation and, optionally, the captured restore view.
- `op restore`: move HEAD and branch refs back to a previously captured view.

## Operation References

`<OP_REF>` may be either:

- A concrete operation id, for example `019e3f00-8ee5-7e62-a54c-0ab1f1bba0f9`
- A reflog-style index, for example `@{0}` for the newest operation or `@{1}`
  for the previous one

## `libra op log`

List operation history.

```bash
libra op log [--page <N>] [-n <PER_PAGE>] [--command <NAME>] [--verbose]
```

### Options

### `-n, --number <PER_PAGE>`

Number of operations to show per page. Defaults to `50`.

```bash
libra op log -n 20
```

### `--page <N>`

Page number to display. Defaults to `1`.

```bash
libra op log --page 2 -n 20
```

### `--command <NAME>`

Filter operations by exact command name, such as `branch` or `op restore`.

```bash
libra op log --command branch
libra op log --command "op restore"
```

### `--verbose`

Show one operation as a multi-line block with actor, status, and timestamp.

```bash
libra op log -n 5 --verbose
```

## `libra op show`

Inspect a single operation.

```bash
libra op show [--view] <OP_REF>
```

### Options

### `--view`

Print the captured restore view, including HEAD target and refs.

```bash
libra op show @{0} --view
```

## `libra op restore`

Restore repository state to a previously captured operation view. HEAD and the
captured branch refs are reset to the target view, and local branches that are
absent from that view are pruned, so the restore reproduces the operation's
exact local-branch set. The restored HEAD branch is always kept; remote-tracking
refs and Libra-owned internal refs (the locked `main`/`intent`/`traces`
branches and the reserved `libra/` namespace, e.g. the AI history branch
`libra/intent`) are never pruned.

```bash
libra op restore [--what <all|working-copy|index|sequencer|sparse|head>] \
  [--confirm-repo-wide] [--force] [--dry-run] <OP_REF>
```

### Options

### `--force`

Allow restore to proceed even if the working tree is dirty.

```bash
libra op restore @{0} --force
```

### `--dry-run`

Show the target HEAD and refs without writing a new restore operation.

```bash
libra op restore @{0} --dry-run
```

### `--what <FACET>`

Select the state facet to restore. The default is `all`; use
`working-copy`, `index`, `sequencer`, `sparse`, or `head` for a selective
restore. v2 restores emit a receipt containing the target view, selected
facets, changed-path count, and (for a real restore) the new operation ID.

### `--confirm-repo-wide`

Explicitly acknowledge a target view containing more than one workspace. The
engine still applies the selected facet only to the pinned worktree and
refuses a target that does not contain that worktree.

Machine consumers can request the receipt with the command's normal `--json`
output mode. A dry-run never writes an operation or changes the worktree.

## `libra op undo`, `redo`, and `revert`

These commands append a new operation; they never rewrite or delete commit
objects. `undo` requires the selected operation to be the unique current head,
and moves to its parent view. `redo` only accepts the current undo head and
replays the source operation recorded by that undo. `revert` requires an
explicit `--parent` operation and applies that parent's view as the inverse.

```bash
libra op undo <OP_REF> [--force] [--confirm-repo-wide] [--dry-run]
libra op redo <UNDO_OP_REF> [--force] [--confirm-repo-wide] [--dry-run]
libra op revert <OP_REF> --parent <PARENT_OP_REF> \
  [--force] [--confirm-repo-wide] [--dry-run]
```

All three commands support JSON receipts. The receipt includes the selected
facets, changed-path count, target view, and the new operation ID when a
transition is published. A dry run computes the same plan without publishing.
Dirty worktrees are refused unless `--force` is supplied.

## Examples

```bash
# List the newest ten operations
libra op log -n 10

# Show only branch operations on page 2
libra op log --command branch --page 2 -n 5

# Inspect the latest operation and its view snapshot
libra op show @{0} --view

# Restore to the previous operation view
libra op restore @{1}

# Preview a restore without changing repository state
libra op restore @{1} --dry-run
```

## Notes

- `op restore` records a new `op restore` operation on success.
- `op restore --dry-run` does not write a new operation.
- Restore resets HEAD and the branch refs captured in the target view, and
  prunes local branches that are absent from that view (the restored HEAD branch
  is always kept; remote-tracking refs are left untouched).
