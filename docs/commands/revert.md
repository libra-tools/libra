# `libra revert`

Revert some existing commits.

## Synopsis

```
libra revert [-n | --no-commit] [-m | --mainline <parent-number>] [-s | --signoff] [-e | --edit] [--no-edit] [--no-rerere-autoupdate] [--json] [--quiet] <commit>...
libra revert --continue
libra revert --skip
libra revert --abort
```

## Description

`libra revert` creates a new commit that undoes the changes introduced by the specified commit. Unlike `reset`, which rewrites history, `revert` is safe for shared branches because it preserves the original commit and adds a new one on top.

The command works by computing the diff between the target commit and its parent, then applying the inverse of that diff to the current working tree and index. If the resulting state is clean, a new commit is recorded with a message of the form `Revert "<original subject>"`.

The revert commit uses the current author and committer identity/date, honoring
`GIT_AUTHOR_*` and `GIT_COMMITTER_*` through the same environment rules as
`libra commit` when it creates the commit. The generated subject is derived from
the target commit's de-signed message body, so an embedded `gpgsig` block is not
used as the original subject.

Reverting a root commit (one with no parent) produces an empty tree, effectively undoing the initial commit's changes.

To revert a **merge commit** (one with more than one parent), pass `-m <parent-number>` to choose which parent is the mainline; the merge's changes are then computed relative to that parent. See [`-m`, `--mainline`](#-m---mainline-parent-number) below.

The command requires an active branch (not detached HEAD). It accepts one or more
commit references, reverting them in order (each as its own revert commit); a
conflict stops the sequence, to be finished with `libra revert --continue`,
skipped with `libra revert --skip`, or undone with `libra revert --abort`. When a
conflict interrupts a multi-commit revert, the commits still pending behind it are
remembered and reverted automatically once `--continue`/`--skip` resumes the
sequence. `-n/--no-commit` and `-m/--mainline` apply only to a single commit.

## Options

### `-n`, `--no-commit`

Apply the inverse changes to the index and working tree but do **not** create a new commit. This is useful when you want to inspect the result or adjust the changes before committing. `--no-commit` applies to a single commit and is rejected when multiple commits are given.

```bash
# Stage the revert without committing
libra revert -n abc1234

# Review what changed
libra diff --cached

# Commit with a custom message
libra commit -m "revert abc1234 with adjustments"
```

### `-m`, `--mainline <parent-number>`

Specify the 1-based parent number to treat as the mainline when reverting a **merge commit**. The merge's changes are computed relative to that parent's tree (so `-m 1` undoes everything the merge brought in relative to the first parent), and the generated revert commit still records a single parent (the current `HEAD`).

```bash
# Revert a merge commit, keeping the first-parent line as the baseline
libra revert -m 1 <merge-commit>
```

- A merge commit **requires** `-m`; omitting it fails with exit 128 (`commit <hash> is a merge but no -m option was given`).
- Passing `-m` for a non-merge commit fails with exit 128 (`mainline was specified but commit <hash> is not a merge`).
- An out-of-range parent number fails with exit 128 (`commit <hash> does not have a parent number <n>`).

### `<commit>...` (positional, required)

One or more commit references to revert, applied in the given order. Each can be a
full SHA-1 hash, an abbreviated hash, a branch name, `HEAD`, or any ref that
resolves to a commit. (The positional is optional only with `--continue`/`--skip`/`--abort`.)

```bash
# Revert the most recent commit
libra revert HEAD

# Revert by hash
libra revert abc1234

# Revert the commit a branch points to
libra revert feature-branch
```

### `--json`

Emit machine-readable JSON output instead of human-readable text. See [Structured Output](#structured-output-json-examples) below.

### `--quiet`

Suppress all human-readable output. Exit code still indicates success or failure.

### `-e`, `--edit`

Open the editor on the auto-generated revert message (`Revert "<subject>"`)
before committing, using the same cascade as `commit` (`$GIT_EDITOR`,
`core.editor`, `$VISUAL`, `$EDITOR`). The edited message has `#` comment lines
stripped and surrounding blank lines trimmed; an empty result aborts the revert.
Unlike Git, Libra's revert does **not** open an editor by default — `--edit` is
opt-in. When a revert conflicts, `--edit` is remembered so `--continue` also
opens the editor. Mutually exclusive with `--no-edit`.

```bash
libra revert HEAD --edit
```

### `--no-edit`

Accept the auto-generated revert message (`Revert "<subject>"`) without launching
an editor. This is Libra's default behavior, so the flag is a no-op accepted for
Git parity; pass `-e`/`--edit` to open the editor instead.

### `--no-rerere-autoupdate`

Do not update the rerere (reuse recorded resolution) index. Accepted no-op for
Git parity: Libra has no rerere, so there is nothing to update. (Git's
`--rerere-autoupdate` is not exposed.)

## Common Commands

```bash
# Revert the most recent commit
libra revert HEAD

# Revert a specific commit by hash
libra revert abc1234

# Revert without auto-committing (to edit or combine)
libra revert -n HEAD

# Revert a merge commit relative to its first parent
libra revert -m 1 <merge-commit>

# Revert with JSON output for AI agents or scripts
libra revert --json HEAD
```

## Human Output

When reverting **with** auto-commit (default):

```
[def5678] Revert commit abc1234
```

When reverting **without** auto-commit (`-n`):

```
Changes staged for revert. Use 'libra commit' to finalize.
```

## Structured Output (JSON examples)

```json
{
  "command": "revert",
  "data": {
    "reverted_commit": "abc1234abcdef1234567890abcdef1234567890ab",
    "short_reverted": "abc1234",
    "new_commit": "def5678abcdef1234567890abcdef1234567890ab",
    "short_new": "def5678",
    "no_commit": false,
    "files_changed": 3
  }
}
```

When `--no-commit` is used, `new_commit` and `short_new` are `null`:

```json
{
  "command": "revert",
  "data": {
    "reverted_commit": "abc1234abcdef1234567890abcdef1234567890ab",
    "short_reverted": "abc1234",
    "new_commit": null,
    "short_new": null,
    "no_commit": true,
    "files_changed": 3
  }
}
```

## Design Rationale (Why different from Git/jj)

### Multiple commits (`<commit>...`)

`libra revert <commit1> <commit2> ...` reverts a sequence of commits in order,
each as its own revert commit relative to the previous result. If a revert in the
sequence conflicts, the operation stops there; already-completed reverts are kept
and the commits still pending behind the conflict are remembered. You then finish
the conflicting one with `libra revert --continue` (after resolving), discard it
with `libra revert --skip`, or undo with `libra revert --abort`; `--continue` and
`--skip` automatically revert the remembered pending commits before completing.
Note that `-n/--no-commit` and `-m/--mainline` apply only to a single commit and
are rejected when multiple commits are given.

### Merge commit support (`--mainline`)

Git's `--mainline <parent-number>` selects which parent of a merge commit to diff against when computing the inverse. Libra supports this: a merge commit (more than one parent) **requires** `-m <n>`, and the revert is computed relative to the chosen parent's tree. To guard against the "picked the wrong parent" footgun:

1. **No silent default.** Omitting `-m` on a merge commit is a hard error (exit 128), so you must consciously choose the mainline rather than getting an arbitrary changeset.
2. **Symmetric guards.** Passing `-m` on a non-merge commit, or a parent number outside the merge's parent count, is also a hard error — the parent ordering must be explicit and valid.

The generated revert commit still records a single parent (the current `HEAD`), matching Git.

### Conflict handling (`--continue`, `--skip`, `--abort`)

A revert that conflicts writes three-way conflict markers to the working tree,
records revert state in `revert-state.json`, and returns `LBR-CONFLICT-001`. You
then resolve the conflicts and run `libra revert --continue` to finish,
`libra revert --skip` to discard the current commit and move on, or
`libra revert --abort` to restore the pre-revert state.

1. **Explicit, agent-friendly errors.** The specific path and error code are
   reported so an agent can resolve the conflict programmatically and continue.
2. **Predictable state.** Revert state lives in a single `revert-state.json`
   file rather than scattered implicit markers.
3. **Sequence-aware.** When the conflict interrupts a multi-commit revert, the
   still-pending commits are stored in the state file (`remaining`), so
   `--continue` (after resolving) and `--skip` (discarding the current commit)
   both finish the rest of the sequence automatically. `--skip` with nothing left
   simply clears the state without creating a commit.

### Conflict model (three-way merge)

Libra's revert applies the inverse change with a path-level three-way merge. When
the result is unambiguous the file is updated cleanly; when it overlaps with later
changes, standard conflict markers are written to the working tree, the unmerged
state and revert progress are saved in `revert-state.json`, and `LBR-CONFLICT-001`
is returned. You resolve the markers and run `libra revert --continue`, skip the
commit with `libra revert --skip`, or unwind with `libra revert --abort`.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Git | jj | Libra |
|-----------|-----|-----|-------|
| Positional commit(s) | `git revert <commit>...` | N/A (uses `jj backout`) | `libra revert <commit>...` (multiple, reverted in order) |
| No-commit mode | `--no-commit` / `-n` | N/A | `--no-commit` / `-n` |
| Accept default message | `--no-edit` | N/A | `--no-edit` (accepted no-op; Libra does not open an editor by default — use `-e`/`--edit` to opt in) |
| No rerere autoupdate | `--no-rerere-autoupdate` | N/A | `--no-rerere-autoupdate` (accepted no-op; no rerere) |
| Edit message | `-e`/`--edit` | N/A | `-e`/`--edit` (open the editor on the message; unlike Git, Libra does not open one by default — opt-in) |
| Mainline parent | `--mainline <n>` / `-m <n>` | N/A | `--mainline <n>` / `-m <n>` (required for merge commits) |
| Continue after conflict | `--continue` | N/A | `--continue` (after resolving; auto-continues remaining commits) |
| Abort in-progress | `--abort` | N/A | `--abort` (restores the pre-revert state) |
| Skip current commit | `--skip` | N/A | `--skip` (discards the conflicted commit, continues the sequence) |
| Strategy | `--strategy <s>` | N/A | Not supported |
| Strategy option | `-X <option>` | N/A | Not supported |
| GPG sign | `--gpg-sign` / `-S` | N/A | Not supported (planned) |
| JSON output | N/A | N/A | `--json` |
| Quiet mode | `--quiet` | N/A | `--quiet` |
| Files changed count | N/A | N/A | Included in JSON output |

**Note:** jj uses `jj backout -r <rev>` as its equivalent to `git revert`. It creates a new commit that is the inverse of the target revision.

## Error Handling

| Code | Condition | Hint |
|------|-----------|------|
| `LBR-REPO-001` | Not inside a libra repository | Initialize with `libra init` or navigate to a repo |
| `LBR-REPO-003` | HEAD is detached (not on a branch) | Switch to a branch with `libra switch <branch>` |
| `LBR-CLI-003` | Cannot resolve the commit reference | Use `libra log` to find valid commit references |
| `LBR-CLI-002` | Merge commit without `-m`, `-m` on a non-merge commit, or out-of-range parent number (exit 128); or, with `-e`/`--edit`, no editor configured, the editor aborted, or an empty message (exit 129) | Pass a valid `-m <parent-number>` for merge commits (omit it for non-merge commits); for `--edit`, set `$GIT_EDITOR`/`core.editor` and save a non-empty message |
| `LBR-CONFLICT-001` | File was modified by a later commit, creating a conflict | Resolve conflicts then `libra revert --continue`, skip the commit with `libra revert --skip`, or cancel with `libra revert --abort` |
| `LBR-IO-001` | Failed to load object (commit, tree, blob) | Check repository integrity |
| `LBR-IO-002` | Failed to save object, index, or update HEAD | Check filesystem permissions and repository writability |
