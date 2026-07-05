# `libra pull`

Fetch objects from a remote and integrate the fetched branch into the current branch.

## Synopsis

```text
libra pull [--ff-only] [--ff] [--no-ff] [--squash] [--no-commit] [--commit] [--autostash] [--no-progress] [--rebase] [--no-rebase] [--depth <n>] [<repository> [<refspec>]]
```

## Description

`libra pull` combines `fetch` and the same merge engine used by `libra merge`. It downloads new objects, updates remote-tracking refs, and then integrates the selected upstream into the current branch.

With `--rebase` (`-r`), the integration step instead replays local-only commits on top of the fetched upstream tip. This is equivalent to `libra fetch` followed by `libra rebase <upstream>`.

With `--ff-only`, pull fetches the upstream but refuses to create a merge commit when local and remote histories have diverged. Fast-forward and already-up-to-date pulls still succeed. `--ff-only` conflicts with `--rebase`, `--ff`, and `--no-ff`.

With `--no-ff`, pull always records a real merge commit even when the upstream could be fast-forwarded, mirroring `git pull --no-ff`. `--ff` explicitly allows the default fast-forward behaviour. `--ff`, `--no-ff`, and `--ff-only` are mutually exclusive and conflict with `--rebase`.

With `--depth <n>`, the fetch phase is limited to a shallow history of `n` commits per tip before integration, mirroring `git pull --depth`. `--depth` is fetch-only and conflicts with `--rebase`.

When invoked with no arguments, the command reads the current branch tracking configuration (`branch.<name>.remote` and `branch.<name>.merge`). When `<repository>` is given alone, the current branch name is used as the remote branch. When both `<repository>` and `<refspec>` are given, the specified remote branch is fetched and merged.

Pull supports already-up-to-date, fast-forward, and single-head three-way merge results. If the local and remote branches conflict, pull returns the merge-owned `LBR-CONFLICT-002` error with `phase: "merge"` and leaves the same merge state that `libra merge` uses. Resolve conflicts with `libra add <path>` and `libra merge --continue`, or run `libra merge --abort`.

With `--squash`, pull fetches and computes the merge but stages the merged tree without creating a commit or moving `HEAD`, leaving the result ready for a plain `libra commit` (mirroring `git pull --squash`). With `--no-commit`, pull performs the merge and stages the result but stops before committing, recording merge state so the two-parent commit can be finalized with `libra merge --continue`. `--squash` and `--no-commit` conflict with each other and with `--rebase`. `--commit` forces a merge commit (the default merge behavior) and is last-one-wins with `--no-commit` (the final flag on the command line decides); it conflicts with `--squash` and `--rebase`, matching `git pull --commit`.

With `--autostash`, pull stashes your tracked working-tree changes before integrating (so a dirty tree does not block the merge/rebase) and re-applies them afterwards — even if the merge/rebase fails. Untracked and ignored files are left in place. If re-applying the stash conflicts, the stash is kept and the failure is reported; recover it with `libra stash pop`.

## Options

| Flag / Argument | Description | Example |
|-----------------|-------------|---------|
| `<repository>` | Remote name to pull from. When omitted, uses the current branch's configured upstream. | `libra pull origin` |
| `<refspec>` | Branch name on the remote. Requires `<repository>`. When omitted, uses the current branch name. | `libra pull origin main` |
| `--ff-only` | Refuse to create a merge commit; succeeds only for fast-forward or already-up-to-date pulls. Conflicts with `--rebase`, `--ff`, `--no-ff`. | `libra pull --ff-only` |
| `--ff` | Explicitly allow a fast-forward merge (the default). Conflicts with `--no-ff`, `--ff-only`, `--rebase`. | `libra pull --ff` |
| `--no-ff` | Always create a merge commit even when a fast-forward is possible. Conflicts with `--ff`, `--ff-only`, `--rebase`. | `libra pull --no-ff` |
| `--squash` | Stage the merged tree without committing or moving `HEAD`, leaving the result for a plain `libra commit`. Conflicts with `--no-commit`, `--rebase`. | `libra pull --squash` |
| `--no-commit` | Merge and stage but stop before committing, recording merge state to finalize with `libra merge --continue`. Conflicts with `--squash`, `--rebase`. | `libra pull --no-commit` |
| `--commit` | Force a merge commit (the default); last-one-wins with `--no-commit`. Conflicts with `--squash`, `--rebase`. | `libra pull --commit` |
| `--autostash` | Stash tracked changes before integrating and re-apply them afterwards (even on failure), so `pull` works on a dirty tree. Untracked/ignored files are left in place. | `libra pull --autostash` |
| `--no-progress` | Suppress the fetch progress meter (the "Receiving objects" spinner), matching `git pull --no-progress`. | `libra pull --no-progress` |
| `--notes` | Forward to the fetch: also import the file-dependency graph (`refs/notes/deps`, lore.md 3.2) from a **local Libra** upstream. Default OFF (Git parity); a network/plain-Git upstream warns and imports nothing (deferred, D17). See `libra fetch --notes`. | `libra pull --notes` |
| `--depth <n>` | Limit the fetch phase to a shallow history of `n` commits per tip. Conflicts with `--rebase`. | `libra pull --depth 1` |
| `-r`, `--rebase` | After fetching, rebase the current branch onto the upstream tip instead of merging. | `libra pull --rebase` |
| `--no-rebase` | Merge instead of rebasing (the default), countermanding an earlier `--rebase`/`-r` (last one wins). Pull merges by default, so on its own this is a no-op. | `libra pull --no-rebase` |
| `--json` | Emit structured JSON envelope to stdout (global flag). | `libra pull --json` |
| `--machine` | Compact single-line JSON; suppresses progress (global flag). | `libra pull --machine` |
| `--quiet` | Suppress all progress and merge summary output. | `libra pull --quiet` |

## Examples

```bash
libra pull
libra pull origin main
libra pull --ff-only
libra pull --no-ff
libra pull --depth 1
libra pull --rebase origin main
```

## Human Output

Default human mode writes fetch progress to `stderr` and the pull summary to `stdout`.

Fast-forward:

```text
From git@github.com:user/repo.git
   abc1234..def5678  origin/main
Updating abc1234..def5678
Fast-forward
 3 files changed
```

Clean three-way merge:

```text
From git@github.com:user/repo.git
   abc1234..def5678  origin/main
Updating abc1234..def5678
Merge made by the 'three-way' strategy.
 2 files changed
```

Already up to date:

```text
From git@github.com:user/repo.git
Already up to date.
```

No tracking information:

```text
There is no tracking information for the current branch.
Please specify which branch you want to merge with.
See git-pull(1) for details.

    libra pull <remote> <branch>

If you wish to set tracking information for this branch you can do so with:

    libra branch --set-upstream-to=origin/<branch> main
```

Rebase:

```text
From git@github.com:user/repo.git
   abc1234..def5678  origin/main
Successfully rebased 2 commits onto 'origin/main' (1111111..2222222).
```

`--quiet` suppresses all progress and merge summary output.

## Structured Output

`--json` writes one success envelope to stdout. `--machine` writes the same schema as one compact JSON line. Success leaves stderr clean.

```json
{
  "ok": true,
  "command": "pull",
  "data": {
    "branch": "main",
    "upstream": "origin/main",
    "fetch": {
      "remote": "origin",
      "url": "git@github.com:user/repo.git",
      "refs_updated": [
        {
          "remote_ref": "refs/remotes/origin/main",
          "old_oid": "abc1234...",
          "new_oid": "def5678..."
        }
      ],
      "objects_fetched": 12,
      "bytes_received": 2048
    },
    "merge": {
      "strategy": "three-way",
      "old_commit": "abc1234...",
      "commit": "def5678...",
      "files_changed": 2,
      "up_to_date": false,
      "parents": ["abc1234...", "fedcba9..."]
    }
  }
}
```

Rebase output omits `merge` and includes `rebase`:

```json
{
  "ok": true,
  "command": "pull",
  "data": {
    "branch": "main",
    "upstream": "origin/main",
    "fetch": {
      "remote": "origin",
      "url": "git@github.com:user/repo.git",
      "refs_updated": [],
      "objects_fetched": 0,
      "bytes_received": 0
    },
    "rebase": {
      "status": "completed",
      "old_commit": "1111111...",
      "commit": "2222222...",
      "replay_count": 2,
      "up_to_date": false
    }
  }
}
```

### Schema Notes

- `branch` is the current local branch being updated.
- `upstream` is the remote tracking branch name, such as `"origin/main"`.
- `fetch.refs_updated` lists remote refs that changed during fetch.
- Exactly one of `merge` or `rebase` is present, depending on whether `--rebase` was passed.
- `merge.old_commit` is the pre-merge `HEAD`; it is `null` on the first pull into an empty local branch.
- `merge.strategy` is `"fast-forward"`, `"three-way"`, or `"already-up-to-date"`.
- `merge.commit` is the new HEAD commit after merge; it is `null` when up to date.
- `merge.parents` appears for successful three-way merge commits.
- `merge.files_changed` is the number of paths changed by the merge result.
- `rebase.status` is `"completed"`, `"fast-forwarded"`, `"already-up-to-date"`, or `"no-commits"`.
- `rebase.replay_count` is the number of local commits replayed onto the upstream tip.
- `rebase.up_to_date` is `true` when the rebase did not move `HEAD`.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Basic pull | `libra pull` | `git pull` | N/A (jj uses `jj git fetch` + working copy) |
| Pull from specific remote | `libra pull origin main` | `git pull origin main` | N/A |
| Fast-forward integration | Supported | Supported | N/A |
| Fast-forward-only pull | `libra pull --ff-only` | `git pull --ff-only` | N/A |
| Three-way integration | Supported through merge engine | Supported | N/A |
| Rebase on pull | `libra pull --rebase` | `git pull --rebase` | N/A |
| Force merge commit | `libra pull --no-ff` | `git pull --no-ff` | N/A |
| Shallow pull | `libra pull --depth 1` | `git pull --depth 1` | N/A |
| Squash | `libra pull --squash` | `git pull --squash` | N/A |
| No-commit | `libra pull --no-commit` (finalize with `libra merge --continue`) | `git pull --no-commit` | N/A |
| Force-commit override | `libra pull --commit` (last-one-wins with `--no-commit`) | `git pull --commit` | N/A |
| Autostash | `libra pull --autostash` | `git pull --autostash` | N/A |
| Suppress progress | `libra pull --no-progress` | `git pull --no-progress` | N/A |
| Structured output | `--json` / `--machine` | No | No |
| Phase diagnostics | `phase` detail in error JSON | No | No |

## Error Handling

Every `PullError` variant maps to an explicit `StableErrorCode`. Fetch, merge, and rebase sub-errors are forwarded with a `phase` detail for diagnostics.

| Scenario | Error Code | Exit | Hint |
|----------|-----------|------|------|
| HEAD is detached | `LBR-REPO-003` | 128 | "checkout a branch before pulling" |
| No tracking info for branch | `LBR-REPO-003` | 128 | Git-style advisory block with `libra pull <remote> <branch>` and `libra branch --set-upstream-to=...` |
| Remote not found | `LBR-CLI-003` | 129 | "use 'libra remote -v' to see configured remotes" |
| Fetch: network unreachable / timeout | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| Fetch: authentication failed | `LBR-AUTH-001` | 128 | "check SSH key or HTTP credentials" |
| Fetch: protocol error | `LBR-NET-002` | 128 | "the remote did not respond correctly" |
| Merge: conflicts, dirty worktree, or untracked overwrite | `LBR-CONFLICT-002` | 128 | "resolve conflicts, then run 'libra merge --continue'" |
| Merge: non-fast-forward rejected by `--ff-only` | `LBR-CONFLICT-002` | 128 | "run 'libra pull' without --ff-only to allow a merge commit" |
| Rebase: conflict during replay | `LBR-CONFLICT-001` | 128 | "resolve conflicts, stage them, then run 'libra rebase --continue'" |
| Rebase: dirty worktree | `LBR-REPO-003` | 128 | "commit or stash your changes before rebasing" |
| Merge: invalid target | `LBR-CLI-003` | 129 | "verify the upstream ref and try again" |
| Merge: unrelated histories or invalid merge state | `LBR-REPO-003` | 128 | "inspect branch history and merge state" |
| Merge: repository corruption | `LBR-REPO-002` | 128 | "inspect repository state and object integrity" |
| Merge: read failure | `LBR-IO-001` | 128 | "check repository metadata and permissions" |
| Merge: write failure | `LBR-IO-002` | 128 | "check filesystem permissions and retry" |

### Phase Detail

When a sub-operation fails, the error JSON includes a `phase` key in the details object (`"fetch"`, `"merge"`, or `"rebase"`) so agents can distinguish which stage failed.
