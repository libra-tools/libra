# `libra cherry-pick`

Apply the changes introduced by some existing commits.

**Alias:** `cp`

## Synopsis

```
libra cherry-pick [-n|--no-commit] [-x] [-s|--signoff] [-e|--edit]
                  [-m <n>|--mainline <n>] [--ff] [-S|--gpg-sign]
                  [--allow-empty] [--allow-empty-message] [--keep-redundant-commits]
                  [--empty=<mode>] [--cleanup=<mode>] [--json] [--quiet] <commit>...
libra cherry-pick (--continue | --skip | --abort | --quit)
```

## Description

`libra cherry-pick` applies the changes introduced by the specified commits onto the current branch. For each named commit, Libra computes the diff between that commit and its parent, applies the resulting changeset to the current index and working tree, and (unless `--no-commit` is given) records a new commit.

This is useful for selectively applying commits from one branch to another without merging. When multiple commits are supplied they are applied in the order given, each one becoming a new commit on the current branch before the next is processed.

The command requires an active branch (not detached HEAD). Non-merge commits are applied directly; merge commits require `-m <parent-number>` to choose which parent to diff against.

Auto-committed picks preserve the source commit's author metadata (name, email,
author date, and timezone). The committer is the current identity/date, honoring
`GIT_COMMITTER_*` overrides. Signed source messages are de-signed before message
cleanup/trailer handling, so `gpgsig` blocks never become the replayed subject.

When a commit cannot be applied cleanly, Libra performs a three-way apply (base = parent tree, ours = current index, theirs = picked tree) and writes any divergent path to the index (stages 1/2/3) and the working tree (line-level conflict markers, matching Git). The in-progress sequence is persisted in the SQLite `cherry_pick_state` table, so you can resolve the conflict and continue with `--continue`, drop the conflicted commit with `--skip`, or undo the whole sequence with `--abort`/`--quit`. While a cherry-pick sequence is in progress, `merge` and `rebase` are blocked (`LBR-CONFLICT-002`).

## Options

### `-n`, `--no-commit`

Apply the changes from the source commit to the index and working tree but do **not** create a new commit. This lets you inspect or combine the changes before committing manually with `libra commit`.

Multiple commits may be supplied; their changes accumulate in the index. Note that `--no-commit` cannot be combined with the conflict sequencer — a conflict during a multi-commit `--no-commit` pick is terminal (clean up with `libra reset --hard`/`libra restore`), since there are no intermediate commits to resume from.

```bash
# Stage changes from abc1234 without committing
libra cherry-pick -n abc1234

# Inspect staged changes, then commit manually
libra status
libra commit -m "cherry-picked and adjusted abc1234"
```

### `-x`

Append `(cherry picked from commit <hash>)` to the new commit message. Without `-x`, Libra keeps the source commit message without adding the provenance line, matching Git's default behavior.

```bash
# Record the original commit hash in the new commit message
libra cherry-pick -x abc1234
```

### `-s`, `--signoff`

Add a `Signed-off-by: <name> <email>` trailer (from the configured `user.name`/`user.email`) to the new commit message. When combined with `-x`, the `(cherry picked from commit ...)` line is emitted first and `Signed-off-by` last, matching Git's trailer ordering.

### `-e`, `--edit`

Open an editor on the assembled commit message before committing. The editor is resolved from `core.editor`, then `$VISUAL`, then `$EDITOR`. In machine/JSON mode or without an interactive TTY, `-e` degrades to the assembled message without launching an editor (so it never blocks automation).

### `-m <n>`, `--mainline <n>`

Cherry-pick a merge commit, using parent number `<n>` (1-based) as the diff base. Required for merge commits — a merge commit without `-m` is rejected. `-m` on a non-merge commit, or an out-of-range parent number, is also rejected (`LBR-CLI-002`).

```bash
# Cherry-pick a merge commit along its first parent
libra cherry-pick -m 1 <merge-commit>
```

### `--ff`

When the picked commit is a single-parent direct child of HEAD and no commit-rewriting modifier is set, fast-forward HEAD to that commit without replaying or rewriting it (no hash drift).

### `-S`, `--gpg-sign`

GPG-sign the cherry-picked commit using the libra vault signing key. Signing happens on explicit request regardless of the `vault.signing` config default. If the vault has no signing key available the pick fails rather than producing an unsigned commit.

### `--allow-empty`

Cherry-pick a commit even if its own change set is empty (its tree equals its parent's). By default such commits are rejected (`LBR-CLI-002`).

### `--allow-empty-message`

Allow the new commit to be created with an empty message. By default an empty message is rejected (`LBR-CLI-002`).

### `--keep-redundant-commits`

Keep a commit that becomes redundant after being replayed (its resulting tree is identical to the current HEAD). By default such redundant commits are rejected (`LBR-CLI-002`). Equivalent to `--empty=keep`.

### `--empty=<mode>`

How to handle a pick whose change set becomes redundant against HEAD after replay. `<mode>` is `stop` (default — halt so you can decide), `drop` (skip the commit; HEAD does not advance and a `dropping <sha> <subject> -- patch contents already upstream` notice is printed), or `keep` (record the now-empty commit; same as `--keep-redundant-commits`). An invalid mode is a usage error (`LBR-CLI-002`, exit 129), validated before any commit (and before `--continue`/`--skip`/`--abort`/`--quit`).

### `--cleanup=<mode>`

Clean up the replayed commit message. `<mode>` is `strip`/`whitespace`/`verbatim`/`scissors`/`default`. The picked body — and any `-e` edited buffer — is cleaned first, then the generated `-x`/`Signed-off-by` trailers are appended (so their separator is preserved). `default` and `scissors` fall back to `whitespace` when no editor opens (matching Git's "if the message is to be edited" clause). An unrecognized mode is a usage error (`LBR-CLI-002`, exit 129), validated before any commit (and before `--continue`/`--skip`/`--abort`/`--quit`). When omitted, the message is trim-only, as before.

## Conflict Sequencer

When a pick conflicts, resolve the affected files, stage them with `libra add`, then continue or cancel:

### `--continue`

Resume the in-progress cherry-pick after resolving conflicts. The index must have no unresolved conflict stages, or `--continue` is refused (`LBR-CONFLICT-001`). Finalizes the conflicted commit and applies any remaining commits in the sequence.

### `--skip`

Drop the current conflicted commit (restoring the worktree to the last successful tip) and continue with the rest of the sequence.

### `--abort`

Cancel the in-progress cherry-pick and reset HEAD/worktree back to the state before the sequence began.

### `--quit`

Forget the in-progress cherry-pick without changing the index or working tree (the conflict markers are left in place).

```bash
# A pick conflicts; resolve, stage, and continue:
libra cherry-pick abc1234 def5678
# ... edit conflicted files ...
libra add <resolved-files>
libra cherry-pick --continue

# Or drop just the conflicted commit:
libra cherry-pick --skip

# Or undo the whole sequence:
libra cherry-pick --abort
```

### `<commit>...` (positional, required)

One or more commit references to cherry-pick. Each value can be a full SHA-1 hash, an abbreviated hash, a branch name, `HEAD`, or any ref that resolves to a commit. Commits are applied left-to-right.

```bash
# Single commit by hash
libra cherry-pick abc1234

# Multiple commits in order
libra cherry-pick abc1234 def5678 ghi9012
```

### `--json`

Emit machine-readable JSON output instead of human-readable text. See [Structured Output](#structured-output-json-examples) below.

### `--quiet`

Suppress all human-readable output. Exit code still indicates success or failure.

## Common Commands

```bash
# Cherry-pick a single commit onto the current branch
libra cherry-pick abc1234

# Cherry-pick multiple commits in sequence
libra cherry-pick abc1234 def5678

# Cherry-pick without committing, to edit or combine changes
libra cherry-pick -n abc1234

# Cherry-pick and record the original commit hash in the new message
libra cherry-pick -x abc1234

# Cherry-pick a merge commit along its first parent, signed off
libra cherry-pick -m 1 -s <merge-commit>

# Resume after resolving a conflict
libra add <resolved-files> && libra cherry-pick --continue

# Cherry-pick with JSON output for AI agents or scripts
libra cherry-pick --json abc1234
```

## Human Output

When cherry-picking **with** auto-commit (default):

```
[def5678] cherry-picked from abc1234
```

When cherry-picking **without** auto-commit (`-n`):

```
Changes from abc1234 staged. Use 'libra commit' to finalize.
```

## Structured Output (JSON examples)

```json
{
  "command": "cherry-pick",
  "data": {
    "picked": [
      {
        "source_commit": "abc1234abcdef1234567890abcdef1234567890ab",
        "short_source": "abc1234",
        "new_commit": "def5678abcdef1234567890abcdef1234567890ab",
        "short_new": "def5678"
      }
    ],
    "no_commit": false
  }
}
```

When `--no-commit` is used, `new_commit` and `short_new` are `null`:

```json
{
  "command": "cherry-pick",
  "data": {
    "picked": [
      {
        "source_commit": "abc1234abcdef1234567890abcdef1234567890ab",
        "short_source": "abc1234",
        "new_commit": null,
        "short_new": null
      }
    ],
    "no_commit": true
  }
}
```

## Design Rationale (Why different from Git/jj)

### Sequencer state lives in SQLite, not dotfiles

Git maintains `.git/CHERRY_PICK_HEAD` and sequencer state files. Libra persists the in-progress sequence in the SQLite `cherry_pick_state` table instead, matching the repository's metadata-in-SQLite convention: the `DELETE`+`INSERT` save runs in a single transaction so a sequencer write is never left half-applied, and there is no loose dotfile that can drift from the refs. The AI-agent protocol is the same as Git's: detect the conflict code (`LBR-CONFLICT-001`), resolve, `libra add`, then `--continue` (or `--skip`/`--abort`/`--quit`).

### Line-level conflict hunks

A divergent path is surfaced with line-level conflict markers, matching Git: a three-way merge (base = parent tree, ours = current index, theirs = picked tree) encloses only the diverging hunks between `<<<<<<< HEAD` / `=======` / `>>>>>>> <short-source>`, leaving lines that both sides share outside the markers. A delete/modify conflict (one side absent) or binary content falls back to a whole-file presentation, where a line-level merge would be meaningless. The `>>>>>>>` label is the picked commit's abbreviation (Libra omits the commit subject Git appends).

The Git-compatible `merge.conflictStyle` config is honored, same as `libra merge`: `diff3` additionally emits the common-ancestor content between a `||||||| base` marker and the `=======` separator; an unsupported value (e.g. `zdiff3`) is a hard error when a conflict must be rendered. See the [merge documentation](merge.md#conflict-style-mergeconflictstyle).

### Unsupported Git options are rejected, not silently ignored

`--cleanup=<mode>` (`strip`/`whitespace`/`verbatim`/`scissors`/`default`) cleans up the replayed commit message: the picked body (and any `-e` edited buffer) is cleaned first, then the generated `-x`/`Signed-off-by` trailers are appended (so their blank-line separator is preserved). `default`/`scissors` fall back to `whitespace` when no editor opens, matching Git's "if the message is to be edited" clause; an unrecognized mode is a usage error (`LBR-CLI-002`, exit 129). `--strategy`, `-X/--strategy-option`, and `--rerere-autoupdate` are explicitly rejected with `LBR-UNSUPPORTED-001` (exit 128) rather than silently ignored, so a script never assumes an unimplemented behavior took effect. Libra uses a single built-in three-way merge; custom merge strategies are out of scope.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Git | jj | Libra |
|-----------|-----|-----|-------|
| Positional commits | `git cherry-pick <commit>...` | N/A (uses `jj rebase`) | `libra cherry-pick <commit>...` |
| No-commit mode | `--no-commit` / `-n` | N/A | `--no-commit` / `-n` (also multi-commit) |
| Record source | `-x` | N/A | `-x` |
| Edit message | `--edit` / `-e` | N/A | `--edit` / `-e` (degrades in machine mode) |
| Sign-off | `--signoff` / `-s` | N/A | `--signoff` / `-s` |
| Mainline parent | `--mainline <n>` / `-m <n>` | N/A | `--mainline <n>` / `-m <n>` |
| Fast-forward | `--ff` | N/A | `--ff` |
| Continue after conflict | `--continue` | N/A | `--continue` |
| Abort in-progress | `--abort` | N/A | `--abort` |
| Skip current commit | `--skip` | N/A | `--skip` |
| Quit sequencer | `--quit` | N/A | `--quit` |
| GPG sign | `--gpg-sign` / `-S` | N/A | `--gpg-sign` / `-S` (via libra vault) |
| Allow empty | `--allow-empty` | N/A | `--allow-empty` |
| Allow empty message | `--allow-empty-message` | N/A | `--allow-empty-message` |
| Keep redundant | `--keep-redundant-commits` | N/A | `--keep-redundant-commits` |
| Cleanup mode | `--cleanup=<mode>` | N/A | `--cleanup=<mode>` (`strip`/`whitespace`/`verbatim`/`scissors`/`default`; cleans the body/edited buffer, then appends trailers) |
| Strategy | `--strategy <s>` | N/A | Rejected (`LBR-UNSUPPORTED-001`) |
| Strategy option | `-X <option>` | N/A | Rejected (`LBR-UNSUPPORTED-001`) |
| Empty mode | `--empty=<mode>` | N/A | `--empty=<mode>` (`stop`/`drop`/`keep`) |
| JSON output | N/A | N/A | `--json` |
| Quiet mode | `--quiet` | `--quiet` | `--quiet` |

**Note:** jj does not have a direct cherry-pick equivalent. The closest operation is `jj rebase -r <rev> -d <dest>`, which moves or copies a commit to a new destination.

## Error Handling

| Code | Condition | Hint |
|------|-----------|------|
| `LBR-REPO-001` | Not inside a libra repository | Initialize with `libra init` or navigate to a repo |
| `LBR-REPO-003` | HEAD detached, no cherry-pick in progress for `--continue`/`--skip`/`--abort`/`--quit`, or `--continue` on the wrong branch | Switch to a branch / start a pick first / switch back to the sequence branch |
| `LBR-CLI-003` | Cannot resolve a commit reference | Use `libra log` to find valid commit references |
| `LBR-CLI-002` | Merge commit without `-m`, invalid/out-of-range `-m`, an invalid `--cleanup` or `--empty` mode, empty commit without `--allow-empty`, redundant commit without `--keep-redundant-commits`/`--empty=drop`/`--empty=keep`, or empty message without `--allow-empty-message` | Use the flag named in the hint |
| `LBR-UNSUPPORTED-001` | An unsupported Git option (`--strategy`, `-X`, `--rerere-autoupdate`) was passed | Drop the option; it is not supported by libra cherry-pick |
| `LBR-CONFLICT-001` | Conflict during cherry-pick (three-way conflict, or untracked file would be overwritten) | Resolve and `libra add`, then `libra cherry-pick --continue` (or `--skip`/`--abort`/`--quit`) |
| `LBR-CONFLICT-002` | `merge`/`rebase` started while a cherry-pick is in progress, or a new pick started over an in-progress sequence | Finish or cancel the cherry-pick first |
| `LBR-IO-001` | Failed to load an object or cherry-pick state | Check repository integrity and retry |
| `LBR-IO-002` | Failed to save object, index, or update branch ref/state | Check filesystem permissions and repository writability |
