# `libra rerere`

**RE**use **RE**corded **RE**solution. Records how you resolved a merge conflict
and replays that resolution automatically when the identical conflict reappears.

## Synopsis

```
libra rerere [status | diff | forget <path>... | clear | gc]
```

## Description

With no subcommand, `rerere` scans the tracked files for conflict markers and:

- records a **preimage** (the conflicted file) for each new conflict, tracking
  it in this worktree's `MERGE_RR` (in the worktree's local gitdir — since
  W2 each worktree tracks its own current conflicts, while the recorded
  resolutions below stay shared across worktrees);
- if a recorded **postimage** (resolution) matches a conflict, **replays** it
  through a three-way merge — writing the result only when that merge is clean;
- once a tracked conflict has been resolved by hand, records its postimage so
  the next identical conflict resolves itself.

A complete conflict is keyed by SHA-256 of its normalized hunk sides: rerere
discards a diff3 base section, orders the two sides lexicographically, and
hashes the resulting side pairs. Labels, side order, and unrelated file context
therefore do not prevent a match. Files without a complete conflict marker fall
back to the historical whole-file hash. Existing whole-file-keyed cache entries
are left in place; a miss is recorded under the normalized key.

| Subcommand | Description |
|------------|-------------|
| (none) | Record preimages / replay resolutions / record postimages. |
| `status` | List the paths whose conflicts are currently tracked. |
| `diff` | Show what changed in each tracked file since its preimage was recorded. |
| `forget <path>...` | Drop the recorded resolution for the given paths. |
| `clear` | Stop tracking the current conflicts (recorded resolutions are kept). |
| `gc` | Prune recorded resolutions older than the thresholds (60 days resolved / 15 days unresolved). |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | Success. |
| `128` | Not inside a repository, `forget` of a path with no recording, or an I/O error. |

## Examples

```bash
# After a merge leaves conflicts, record them
libra rerere

# Resolve the files by hand, then let rerere learn the resolution
libra rerere

# The next time the same conflict appears, rerere resolves it for you
libra rerere status
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Record / replay | `libra rerere` | `git rerere` |
| Inspect | `libra rerere status` / `diff` | `git rerere status` / `diff` |
| Drop / reset | `libra rerere forget <p>` / `clear` / `gc` | `git rerere forget <p>` / `clear` / `gc` |

## Automatic integration

When `rerere.enabled` is `true`, `merge`, `rebase`, and `cherry-pick` run rerere
for you — there is no need to invoke `libra rerere` by hand:

- when a conflict is written, the preimage is recorded and, if the identical
  conflict was resolved before, the resolution is **replayed** into the working
  tree;
- when the conflict is resolved and the operation is committed / `--continue`d,
  the postimage (your resolution) is recorded.

Enable it with:

```
libra config rerere.enabled true
```

With `rerere.enabled` unset (the default) these hooks are complete no-ops, so
those commands behave exactly as before.

Staging of a replayed file follows `rerere.autoUpdate` (`libra config
rerere.autoUpdate true`). `merge`, `rebase`, and `cherry-pick` all accept
`--rerere-autoupdate` to stage a replayed resolution and
`--no-rerere-autoupdate` to leave it unstaged; the last supplied flag wins and
omitting both inherits the configuration. The explicit choice survives an
interrupted operation's `--continue`.

Rerere stores the complete normalized preimage as the three-way base and the
recorded resolution as the other side. It merges those with the current
normalized conflict before replacing a file; if the replay itself conflicts,
the working-tree file is left unchanged.
