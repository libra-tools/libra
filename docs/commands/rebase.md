# `libra rebase`

Reapply commits on top of another base tip.

**Alias:** `rb`

## Synopsis

```
libra rebase <upstream>
libra rebase [--autosquash] [--reapply-cherry-picks] [--autostash] [--exec <cmd>] [--update-refs] [--fork-point] [--no-rerere-autoupdate] [--keep-empty | --no-keep-empty] [--empty=<mode>] <upstream>
libra rebase --onto <newbase> <upstream> [<branch>]
libra rebase --continue
libra rebase --abort
libra rebase --skip
```

## Description

`libra rebase` moves a sequence of commits from the current branch onto a new base commit. It finds the common ancestor between the current branch and the specified upstream, collects all commits from that ancestor to the current HEAD, and replays each commit on top of the upstream branch. After all commits are replayed, the current branch reference is updated to point to the final rebased commit.

If a conflict occurs during replay, the rebase stops and reports the conflicting files. The user resolves conflicts manually, stages the resolved files, and then runs `libra rebase --continue` to proceed. Alternatively, `--abort` restores the original branch state and `--skip` discards the current commit and moves on to the next.

With `--autosquash`, commits whose subject starts with `fixup!`, `squash!`, or `amend!` are moved next to the matching target commit and folded while replaying. Fixup commits keep the target commit message, squash commits append their message to the target message, and amend commits replace the target message with the amend commit message. `--reapply-cherry-picks` is accepted as an explicit request to keep Libra's default behavior of replaying clean cherry-pick commits.

`--autostash` preserves tracked index and worktree changes in a held stash object before replay and restores the staged index and unstaged worktree layers separately after success or abort. `--exec <cmd>` runs each repeatable command after every replayed commit through Libra's required workspace-write, network-denied sandbox; a failure stops the sequence and `--continue` retries the failed command. `--skip` after an exec failure keeps the already replayed commit and skips the remaining commands for that commit. `--update-refs` atomically retargets other local branches in the rewritten range, except branches checked out in any worktree. `--fork-point` uses the upstream reflog to recover the most specific old upstream tip that remains an ancestor of `HEAD`, then falls back to the ordinary merge base.

Rebase state (the list of remaining and completed commits, the original HEAD, and the target base) is persisted in the SQLite database. Recovery-critical autostash, exec, and update-refs metadata is fsynced atomically in `.libra/rebase-aux.json` until the sequence reaches a terminal state. Legacy file-based state from older Libra versions is automatically migrated to the database on first access.

## Options

| Option | Long | Description |
|--------|------|-------------|
| `<upstream>` | | The upstream branch or commit to rebase onto. Required unless `--continue`, `--abort`, or `--skip` is specified. Can be a branch name, commit hash, or any Git reference. |
| | `--onto <newbase>` | Replay the `<upstream>..HEAD` range onto `<newbase>` instead of onto `<upstream>`. |
| | `--continue` | Continue the rebase after resolving conflicts. Mutually exclusive with `--abort`, `--skip`, and `<upstream>`. |
| | `--abort` | Abort the current rebase and restore the original branch to its pre-rebase state. Mutually exclusive with `--continue`, `--skip`, and `<upstream>`. |
| | `--skip` | Skip the current conflicting commit, or skip the remaining commands after an exec failure while keeping the replayed commit. Mutually exclusive with `--continue`, `--abort`, and `<upstream>`. |
| | `--autosquash` | Move and fold `fixup!`, `squash!`, and `amend!` commits into their target commits during replay. |
| | `--reapply-cherry-picks` | Explicitly replay clean cherry-pick commits. This matches Libra's default linear replay behavior. |
| | `--autostash` / `--no-autostash` | Stash tracked index/worktree changes before replay, preserving the staged and unstaged layers separately, and restore them after success or abort. A conflicting restore is preserved as `stash@{0}` with a warning. The last toggle wins. |
| | `--exec <cmd>` | Run a repeatable shell command after each replayed commit in a required workspace-write, network-denied sandbox. Non-zero exit or timeout stops the rebase; `--continue` retries it. |
| | `--update-refs` / `--no-update-refs` | Atomically move other local branches that point into the rewritten range. Branches checked out in any worktree are excluded. The last toggle wins. |
| | `--fork-point` / `--no-fork-point` | Select the replay boundary from the upstream reflog when possible, otherwise use the ordinary merge base. The last toggle wins. |
| | `--no-rerere-autoupdate` | Accepted no-op for Git parity: rerere recording is integrated when enabled, but rebase does not expose positive `--rerere-autoupdate`; staging follows `rerere.autoUpdate`. |
| | `--keep-empty` | Keep commits that begin empty (already empty before replay) rather than dropping them. Accepted no-op for Git parity: Libra's rebase already keeps empty commits by default. Toggle pair with `--no-keep-empty`; the last one wins. |
| | `--no-keep-empty` | Drop commits that begin empty (their tree equals their parent's — they introduce no change) instead of replaying them. Toggle pair with `--keep-empty`. (This controls commits that *begin* empty; `--empty=<mode>` controls commits that *become* empty after replay.) |
| | `--empty=<mode>` | How to handle a commit that *becomes* empty after replay (its change is already on the new base): `drop` skips it (HEAD does not advance; a `dropping <sha> <subject> -- patch contents already upstream` notice is printed), `keep` records the empty commit. Omitted, Libra **keeps** it — an intentional divergence from Git, which drops by default; pass `--empty=drop` for Git's behavior. The mode survives a conflict into `--continue`/`--skip`. Git's `stop`/`ask` (halt for you to decide) are not supported (Libra's non-interactive rebase has no halt-on-empty resume flow); they and any unknown value are usage errors (`LBR-CLI-002`, exit 129). |

### Option Details

**`<upstream>`**

Start a new rebase, replaying current branch commits onto the specified upstream:

```bash
$ libra rebase main
Found common ancestor: abc1234
Rebasing 3 commits from `feature` onto `main`...
Applied: def5678 feat: add parser
Applied: 987abcd feat: add lexer
Applied: 13579bd test: add parser tests
Successfully rebased branch 'feature' onto '1234567'.
```

**`--autostash`, `--exec`, `--update-refs`, and `--fork-point`**

```bash
# Preserve tracked local changes around the rewrite.
libra rebase --autostash main

# Run both commands, in order, after every replayed commit.
libra rebase --exec 'cargo test' --exec 'cargo clippy' main

# Retarget non-checked-out local branches in the rewritten range.
libra rebase --update-refs main

# Use a force-moved upstream's reflog to avoid replaying old upstream commits.
libra rebase --fork-point origin/main
```

Exec commands are user-controlled shell input. Libra runs them only when its internal sandbox can enforce workspace-only writes and denied network access; if the required backend is unavailable, the command fails closed with `LBR-CONFLICT-002` and leaves the rebase resumable.

**`--continue`**

After resolving conflicts and staging the resolved files, continue the rebase:

```bash
$ libra rebase --continue
Applied: 987abcd feat: add lexer
Rebasing 1 commits from `feature` onto `1234567`...
Applied: 13579bd test: add parser tests
Successfully rebased branch 'feature' onto '1234567'.
```

**`--abort`**

Abort the rebase and restore the original branch state:

```bash
$ libra rebase --abort
Rebase aborted. Restored branch 'feature'.
```

**`--skip`**

Skip the current conflicting commit and move to the next one:

```bash
$ libra rebase --skip
Skipped: 987abcd feat: add lexer
Rebasing 1 commits from `feature` onto `1234567`...
Applied: 13579bd test: add parser tests
Successfully rebased branch 'feature' onto '1234567'.
```

**`--autosquash`**

Fold fixup and squash commits while rebasing:

```bash
$ libra rebase --autosquash main
Found common ancestor: abc1234
Rebasing 2 commits from `feature` onto `main`...
Applied: def5678 feat: add parser
Applied: 13579bd fixup! feat: add parser
Successfully rebased branch 'feature' onto '1234567'.
```

## Common Commands

```bash
# Rebase current branch onto main
libra rebase main

# Rebase onto a specific commit
libra rebase abc1234

# Fold fixup!/squash! commits into their targets
libra rebase --autosquash main

# Explicitly keep replaying clean cherry-picks
libra rebase --reapply-cherry-picks main

# Preserve tracked local changes around the rebase
libra rebase --autostash main

# Run a sandboxed check after each replayed commit
libra rebase --exec 'cargo test' main

# Retarget other local branches in the rewritten range
libra rebase --update-refs main

# Recover the fork point after an upstream force-move
libra rebase --fork-point origin/main

# Transplant the dev..HEAD range onto main (keep the range, move the base)
libra rebase --onto main dev

# Same, naming the branch to rebase as the third positional
libra rebase --onto main dev topic

# Continue after resolving conflicts
libra rebase --continue

# Abort the rebase
libra rebase --abort

# Skip a problematic commit
libra rebase --skip

# Using the alias
libra rb main
```

## Human Output

Normal rebase progress:

```text
Found common ancestor: abc1234
Rebasing 3 commits from `feature` onto `main`...
Applied: def5678 feat: add parser
Applied: 987abcd feat: add lexer
Applied: 13579bd test: add parser tests
Successfully rebased branch 'feature' onto '1234567'.
```

Conflict during rebase:

```text
fatal: rebase stopped while applying 987abcd: feat: add lexer

Hint: conflicted files:
Hint:   src/lexer.rs
Hint: resolve conflicts, stage them, then run 'libra rebase --continue'.
Hint: or run 'libra rebase --skip' / 'libra rebase --abort'.
```

Already up to date:

```text
Current branch is ahead of upstream. No rebase needed.
```

Fast-forward-only case:

```text
Fast-forwarded branch 'feature' to 'main'.
```

Abort:

```text
Rebase aborted. Restored branch 'feature'.
```

## JSON / Machine Output

`--json` and `--machine` are currently supported for successful `rebase <upstream>`, `--abort`, `--continue`, and `--skip` output. CLI/preflight failures, unresolved-conflict `--continue` failures, and structured `rebase <upstream>` conflict stops are rendered through Libra's standard structured error envelope. Deeper replay/conflict-stop error taxonomy is still tracked as follow-up work in the command improvement plan.

Start and complete a replay:

```json
{
  "ok": true,
  "command": "rebase",
  "data": {
    "action": "start",
    "status": "completed",
    "branch": "feature",
    "commit": "abc1234...",
    "upstream": "main",
    "onto": "fedcba9...",
    "common_ancestor": "0123456...",
    "replay_count": 1,
    "previous_commit": "def5678...",
    "applied_commits": [
      {
        "original_commit": "0123456...",
        "commit": "abc1234...",
        "subject": "Feature adds file"
      }
    ],
    "remaining": 0
  }
}
```

Fast-forward start results use the same envelope with `status: "fast-forwarded"`, `commit` equal to `onto`, and no `applied_commits`. Branches already ahead of upstream return `status: "already-up-to-date"`.

```json
{
  "ok": true,
  "command": "rebase",
  "data": {
    "action": "abort",
    "status": "aborted",
    "branch": "feature",
    "commit": "abc1234...",
    "previous_commit": "def5678...",
    "restored": true
  }
}
```

Continue after resolving a conflict:

```json
{
  "ok": true,
  "command": "rebase",
  "data": {
    "action": "continue",
    "status": "completed",
    "branch": "feature",
    "commit": "abc1234...",
    "onto": "fedcba9...",
    "previous_commit": "def5678...",
    "applied_commits": [
      {
        "original_commit": "0123456...",
        "commit": "abc1234...",
        "subject": "Feature modifies conflict.txt"
      }
    ],
    "remaining": 0
  }
}
```

Skip the stopped commit (after an exec failure, `skipped_commit` is absent because the replayed commit is kept):

```json
{
  "ok": true,
  "command": "rebase",
  "data": {
    "action": "skip",
    "status": "completed",
    "branch": "feature",
    "commit": "abc1234...",
    "onto": "fedcba9...",
    "previous_commit": "def5678...",
    "skipped_commit": "0123456...",
    "skipped_subject": "Feature modifies conflict.txt",
    "remaining": 0
  }
}
```

## Rebase State Persistence

Rebase state is stored in a `rebase_state` SQLite table with the following fields:

| Field | Type | Description |
|-------|------|-------------|
| `head_name` | TEXT | Original branch name being rebased |
| `onto` | TEXT | Commit hash being rebased onto |
| `orig_head` | TEXT | Original HEAD commit before rebase started |
| `current_head` | TEXT | Current new base (HEAD of rebased commits so far) |
| `todo` | TEXT | Remaining commits to replay (newline-separated hashes) |
| `todo_actions` | TEXT | Remaining replay actions (newline-separated `pick` / `fixup` / `squash` / `amend`) |
| `done` | TEXT | Commits already replayed (newline-separated hashes) |
| `stopped_sha` | TEXT (nullable) | Current commit that caused a conflict |
| `autosquash` | INTEGER | Whether the current rebase folds autosquash commits (`0` or `1`) |

`.libra/rebase-aux.json` is an atomic, always-fsynced recovery sidecar containing repeatable exec commands and the pending command index, captured update-refs branches and rewrite mappings, and the held autostash object ID. It survives conflicts, exec failures, process restarts, and `maintenance gc` (which treats the held OID as a fail-closed reachability root). Final branch updates use one SQLite transaction and compare captured old tips before moving any branch; concurrent branch movement fails closed. Checked-out branches are never captured. The sidecar is removed only after refs, worktree/index, sequencer state, and autostash restoration have reached a terminal state. If autostash re-application conflicts, the object is first promoted to the normal stash list so the local changes remain recoverable.

## Design Rationale

### Why no `--interactive` / `-i`?

Git's interactive rebase opens an editor with a list of commits that can be reordered, squashed, edited, or dropped. This is one of Git's most powerful features but is inherently interactive: it requires an editor session and human decision-making at launch time.

Libra targets AI-agent and automation workflows where interactive editor sessions are not feasible. Instead of interactive rebase, Libra encourages breaking complex history rewriting into discrete operations: use `rebase` for linear replay, and (in the future) dedicated commands for squashing or reordering.

### Using `--onto`

`libra rebase --onto <newbase> <upstream> [<branch>]` replays the commit range
`<upstream>..HEAD` onto `<newbase>` instead of onto `<upstream>`. The **range of
commits replayed is unchanged** (still `<upstream>..HEAD`); only the landing
point moves. This is the classic "transplant a topic branch onto a different
base" operation:

```bash
# Move the commits unique to `topic` (relative to `dev`) onto `main`.
libra switch topic
libra rebase --onto main dev
```

The optional third positional `<branch>` checks that branch out first, so
`libra rebase --onto main dev topic` is equivalent to
`libra switch topic && libra rebase --onto main dev`.

Notes and current limitations:

- When `--onto` is given, the fast-forward / already-up-to-date short-circuits
  are skipped — an explicit landing point always replays (even when `<upstream>`
  is an ancestor of `HEAD`). An **empty** `<upstream>..HEAD` range still does
  nothing and leaves the branch where it is.
- The replay range is computed first-parent only (merge commits in the range are
  not preserved), matching plain `libra rebase`. Use `--onto` on linear or
  simple-fork history.
- `<upstream>` must be given explicitly; Libra does not infer it from an upstream
  tracking branch.

### Why persist state in SQLite?

Git persists rebase state in a `.git/rebase-merge/` directory with one file per field (head-name, onto, orig-head, etc.). This approach is fragile: partial writes can corrupt state, and concurrent access has no protection.

Libra uses SQLite for rebase state persistence, which provides:
- **Atomic writes**: State updates are transactional, preventing partial corruption.
- **Consistent reads**: No torn reads from partially-written files.
- **Schema evolution**: New fields can be added with migrations rather than new files.
- **Single source of truth**: All metadata lives in one database, simplifying backup and restore.

### How does this compare to Git and jj?

Git's rebase is feature-rich with interactive mode, autosquash, `--onto`, `--exec`, `--rebase-merges`, and more. It is one of the most complex commands in Git, with numerous edge cases around conflict resolution and state management.

jj takes a fundamentally different approach: history is immutable by default, and there is no rebase command. Instead, `jj rebase` exists but operates on the revision DAG directly, moving revisions and their descendants to a new parent. Conflicts are recorded in the commit itself rather than stopping the process, so there is no `--continue`/`--abort` flow.

Libra provides a middle ground: a linear rebase with conflict-stop semantics (familiar to Git users) but with SQLite-backed state persistence for reliability.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Upstream | `<upstream>` (positional) | `<upstream>` (positional) | `-d` / `--destination` |
| Continue | `--continue` | `--continue` | N/A (conflicts stored in commit) |
| Abort | `--abort` | `--abort` | `jj op undo` |
| Skip | `--skip` | `--skip` | N/A |
| Interactive | Not supported | `-i` / `--interactive` | N/A |
| Onto | `--onto <newbase>` | `--onto <newbase>` | `-d` with `-s` / `--source` |
| Exec | Supported; repeatable, required workspace-write/network-denied sandbox, resumable failure | `--exec <cmd>` | N/A |
| Autosquash | Supported | `--autosquash` | N/A |
| Autostash | `--autostash` / `--no-autostash` supported; tracked changes held through sequencer stops | `--autostash` / `--no-autostash` | N/A |
| Update refs | Supported; checked-out branches excluded and captured tips compared atomically | `--update-refs` / `--no-update-refs` | N/A |
| Fork point | Supported with upstream-reflog selection and merge-base fallback | `--fork-point` / `--no-fork-point` | N/A |
| Rerere autoupdate | `--no-rerere-autoupdate` accepted no-op; positive flag not exposed, staging follows `rerere.autoUpdate` | `--rerere-autoupdate` / `--no-rerere-autoupdate` | N/A |
| Reapply cherry-picks | Supported; Libra replays by default | `--reapply-cherry-picks` | N/A |
| Rebase merges | Not supported | `--rebase-merges` | Default behavior |
| Keep empty | `--keep-empty` (no-op; already keeps empty) / `--no-keep-empty` (drop start-empty commits) | `--keep-empty` / `--no-keep-empty` | Default keeps empty |
| Empty mode | `--empty=<drop\|keep>` (become-empty; default **keep**) | `--empty=<drop\|keep\|stop>` (default drop) | N/A |
| Force rebase | Not supported | `--force-rebase` | N/A |
| Branch | `<branch>` (third positional) | `<branch>` (third positional) | `-s` / `--source` |
| Revision set | Not supported | N/A | `-r` / `--revisions` |
| State persistence | SQLite database | `.git/rebase-merge/` directory | Not applicable |

Note: jj does not stop on conflicts during rebase. Instead, conflicts are materialized in the commit content and can be resolved later, which eliminates the need for `--continue`/`--abort`/`--skip`.

## Error Handling

`execute_safe` and the replay controls return standard structured `CliError` envelopes for CLI, state, conflict, sandbox, and durable-sidecar failures.

| Scenario | StableErrorCode | Exit | Behavior |
|----------|-----------------|------|----------|
| Not a libra repository | `LBR-REPO-001` (RepoNotFound) | 128 | Error with repo-not-found message |
| Missing upstream | `LBR-CLI-002` (CliInvalidArgument) | 129 | Usage error from clap |
| Upstream ref cannot be resolved | `LBR-CLI-003` (CliInvalidTarget) | 129 | Error indicating the ref is not valid |
| `--continue` without rebase in progress | `LBR-REPO-003` (RepoStateInvalid) | 128 | Error indicating no rebase in progress |
| `--continue` with unresolved conflicts | `LBR-CONFLICT-001` (ConflictUnresolved) | 128 | Error indicating conflicts must be staged with `libra add <file>` |
| `--abort` without rebase in progress | `LBR-REPO-003` (RepoStateInvalid) | 128 | Error indicating no rebase in progress |
| `--skip` without rebase in progress | `LBR-REPO-003` (RepoStateInvalid) | 128 | Error indicating no rebase in progress |
| `--skip` without stopped or pending commit | `LBR-REPO-003` (RepoStateInvalid) | 128 | Error indicating there is no commit to skip |
| Empty or NUL-containing `--exec` command | `LBR-CLI-002` (CliInvalidArguments) | 129 | Rejected before rebase state or worktree mutation |
| Exec failure, timeout, or unavailable required sandbox | `LBR-CONFLICT-002` (ConflictOperationBlocked) | 128 | Rebase remains resumable; fix and `--continue`, or `--skip` the remaining exec commands |
| Autostash application conflict | warning; held object promoted to `stash@{0}` | 0 | Rebase completes without losing local changes; inspect the stash |
| Update-refs branch moved concurrently | `LBR-IO-002` (IoWriteFailed) | 128 | Ref transaction rolls back and the rebase remains resumable |
| No common ancestor found | pending typed mapping | 128 | Legacy text error refusing to rebase unrelated histories |
| Conflict during commit replay | pending typed mapping | 128 | Rebase stops, state is saved, user prompted to resolve |
| Failed to create rebased commit | pending typed mapping | 128 | Legacy text error with commit details |
| Failed to update branch reference | pending typed mapping | 128 | Legacy text error with ref update details |
