# `libra pull`

Fetch objects from a remote and integrate the fetched branch into the current branch.

## Synopsis

```text
libra pull [--ff-only] [--ff] [--no-ff] [--squash] [--no-commit] [--commit] [--autostash] [--no-progress] [--rebase] [--no-rebase] [--depth <n>] [<repository> [<refspec>]]
```

## Description

`libra pull` combines `fetch` and the same merge engine used by `libra merge`. It downloads new objects, updates remote-tracking refs, and then integrates the selected upstream into the current branch. A remote URL that points at a Git v2 bundle is re-read on every pull, so replacing the bundle file can fast-forward the current branch.

With `--rebase` (`-r`), the integration step instead replays local-only commits on top of the fetched upstream tip. This is equivalent to `libra fetch` followed by `libra rebase <upstream>`.

With `--ff-only`, pull fetches the upstream but refuses to create a merge commit when local and remote histories have diverged. Fast-forward and already-up-to-date pulls can succeed when the integration preflight allows them; an unresolved index still blocks the merge phase. `--ff-only` conflicts with `--rebase`, `--ff`, and `--no-ff`; like Git, it may be combined with `--squash`, `--no-commit`, or `--commit`.

With `--no-ff`, pull always records a real merge commit even when the upstream could be fast-forwarded, mirroring `git pull --no-ff`. `--ff` explicitly allows a fast-forward and overrides `pull.ff` for the invocation. `--ff`, `--no-ff`, and `--ff-only` are mutually exclusive and conflict with `--rebase`; each may be combined with `--commit`.

Merge-only flags (`--ff-only`, `--ff`, `--no-ff`, `--squash`, `--no-commit`, and `--commit`) also select the merge path when `pull.rebase` or `branch.<name>.rebase` is configured. Contradictory explicit merge flags are rejected before fetch.

With `--depth <n>`, the fetch phase is limited to a shallow history of `n` commits per tip before integration, mirroring `git pull --depth`. `--depth` is fetch-only and conflicts with `--rebase`. The selected upstream must support shallow-boundary negotiation; a local Libra upstream fails closed with `LBR-REPO-002` before integration.

When command-line integration flags are omitted, Libra honors Git-style pull defaults
from local, global, then system config (variable names are case-insensitive):
`branch.<name>.rebase` overrides `pull.rebase`, and `pull.ff` accepts `true`, `false`,
or `only`. CLI flags still take precedence. Local and global encrypted values are
decrypted before validation. `pull.rebase=merges`/`interactive` (and `m`/`i`) are
recognized Git modes but are explicitly unsupported by Libra pull and fail with an
actionable `LBR-CLI-002` diagnostic. Empty or otherwise invalid local/global configured
values fail before fetch or integration with `LBR-CLI-002`; local/global config read
failures use `LBR-IO-001`. An unreadable or unsupported system config scope is skipped,
so a lower-precedence default or the built-in merge behavior can still be used.

When invoked with no arguments, the command reads the current branch tracking configuration (`branch.<name>.remote` and `branch.<name>.merge`). A configured local upstream (`branch.<name>.remote=.`, written by `libra branch -u <local-branch>`) is refused before any network or `FETCH_HEAD` write (`LBR-CLI-003`, exit 129); Git 2.54 `fetch`/`pull` can operate on a local upstream — that support is deferred to [issues/480 HP-16](https://github.com/libra-tools/libra/issues/480). An explicit repository argument `.` keeps the existing `remote '.' not found` path. When `<repository>` is given alone, the current branch name is used as the remote branch. When both `<repository>` and `<refspec>` are given, the specified remote branch is fetched and merged.

Before its merge phase starts, pull inherits `libra merge`'s index check. If there is no merge state but unresolved entries remain (for example after a conflicted squash), the merge phase fails with `LBR-CONFLICT-002` (exit 128, `phase: "merge"`) even when the fetched target is already up to date. That phase preserves HEAD, the index and working tree, and directs you to resolve the conflicts, stage them with `libra add`, then make a plain `libra commit`. Existing merge state retains the usual `merge --continue` / `--abort` guidance. Fetch may already have downloaded objects and updated remote-tracking refs before this merge-phase refusal.

Pull supports already-up-to-date, fast-forward, and single-head three-way merge results — including criss-cross histories with several merge bases, which the shared merge engine resolves through a recursive virtual ancestor exactly as `libra merge` does (see its "Criss-cross histories" section; a history nesting deeper than 20 levels, or with more than 32 merge bases at one level, is refused with `LBR-UNSUPPORTED-001`). If the local and remote branches conflict, pull returns the merge-owned `LBR-CONFLICT-002` error with `phase: "merge"` and, unless `--squash` was requested, leaves the same merge state that `libra merge` uses. For a non-squash conflict, resolve and stage the files with `libra add <path>` and run `libra merge --continue`, or run `libra merge --abort`. A squash conflict instead leaves unresolved index entries and no merge state: resolve and stage the files, then run a plain `libra commit`. Renames are detected per side exactly as `libra merge` detects them (see its "Renames" section), so a file the remote renamed while you changed it merges at the new path; `merge.renames` and `merge.renameLimit` apply here too. The path-level rename conflicts are inherited unchanged as well — `CONFLICT (rename/rename)`, `CONFLICT (rename/delete)` and `CONFLICT (rename involved in collision)` are printed before the conflict error in human output (under `--json`/`--machine` stdout stays machine-clean), and both sides renaming a file to the same path merges cleanly rather than conflicting. Directory/file collisions are settled the way `libra merge` settles them (see its "Directory/file collisions" section): the directory keeps the path, the file is moved to `<path>~HEAD` / `<path>~<branch>` and Git's `CONFLICT (file/directory)` line is printed before the conflict error in human output (under `--json`/`--machine` stdout stays machine-clean and only the error envelope reports the conflict).

Pull's merge phase also inherits directory-rename inference and
`merge.directoryRenames=true|false|conflict` from `libra merge`. The default
`conflict` mode suggests and stages the relocated path as an unresolved merge;
`true` relocates it automatically, and `false` leaves additions below the old
directory name. Split destinations stop the merge while leaving affected
additions at stage 0. Unknown values fail closed in the merge phase with
`LBR-REPO-003`; fetch may already have updated remote-tracking refs before
that phase-local refusal.

The merge path also inherits `merge.renormalize=true|false`. When enabled, the
shared three-way engine canonicalizes `text` / `eol`-attributed base, local, and
remote inputs before content merge, then preserves the local (ours-side) line
endings in the result. Pull exposes no `-X renormalize|no-renormalize` override;
use repository configuration. An invalid value fails with `LBR-REPO-003` only
if pull reaches a real three-way merge. Fetch may already have updated its
objects and remote-tracking refs; fast-forward and already-up-to-date
integration do not consume the setting. Arbitrary clean/smudge filters remain
unsupported.

With `--squash`, pull fetches and computes the merge but stages the merged tree without creating a commit or moving `HEAD`, leaving the result ready for a single-parent plain `libra commit` (mirroring `git pull --squash`). Even on conflict, no merge state is recorded and HEAD stays unchanged; resolve and stage the files before that ordinary commit. `merge --continue`, `--abort` and `--restart` report `no merge in progress`. With `--no-commit`, pull performs the merge and stages the result but stops before committing, recording merge state so the two-parent commit can be finalized with `libra merge --continue`. `--squash` and `--no-commit` conflict with each other and with `--rebase`. `--commit` requests that the merge be committed and is last-one-wins with `--no-commit` (the final flag decides); it does not itself force a merge commit or override the selected fast-forward policy, and conflicts only with `--squash` and `--rebase`.

With `--autostash`, pull stashes your tracked working-tree changes before integrating (so a dirty tree does not block the merge/rebase) and re-applies them when the integration concludes. What "concludes" means depends on the path: the **rebase** path restores them even when the rebase fails; the **merge** path uses `libra merge`'s own autostash, which re-applies on a clean result or an up-front failure but **holds** the stash (JSON `autostash: "kept"`) while a conflicted non-squash merge is in progress — it is re-applied by `libra merge --continue` or `libra merge --abort`, never lost. A conflicted squash instead saves its autostash directly in `stash list` without applying it, preserving the unresolved index and working tree; resolve and commit the squash, then run `libra stash pop`. Saving failure warns and preserves the held sidecar reference. Untracked and ignored files are left in place. If re-applying the stash conflicts, the stash is promoted to the stash list and the failure is reported; recover it with `libra stash pop`.

## Global Config Schema Guard

Configuration schema compatibility is role-scoped. Before `libra pull` trusts
configuration, it inspects GlobalConfig and SystemConfig metadata read-only. A
future configuration schema or an unregistered/mismatched migration receipt
fails closed with `LBR-CONFIG-001` when that scope is required. Known
Repository-only receipts, including `2026090801` in the current manifest, do
not make a configuration store future; its supported values remain readable.
The configuration-owned legacy-reader barrier is recognized by this build;
see [configuration compatibility](config.md#configuration-schema-compatibility).

Global configuration uses `LIBRA_CONFIG_GLOBAL_DB` or the XDG configuration
directory (`$XDG_CONFIG_HOME/libra/config.db`, defaulting to
`<home>/.config/libra/config.db`), falling back to the legacy
`<home>/.libra/config.db` until it is migrated;
system configuration uses `LIBRA_CONFIG_SYSTEM_DB` or `/etc/libra/config.db`.
Complete process/repo-local storage settings can make GlobalConfig unnecessary
(`cloud` must also satisfy its D1 settings). They do not prove that SystemConfig
defaults are unnecessary. Diagnostics identify the affected scope, ledger and
version without printing configuration values or untrusted receipt names.

Unknown or unsupported state is upgrade-only here, not automatically repaired.
Install a compatible newer Libra binary:
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`.
Do not delete or edit SQLite receipts manually. Use `--offline` or
`LIBRA_READ_POLICY=offline|local` only for intentional local-only object access;
these modes warn and are not authorization for remote synchronization.

## Options

| Flag / Argument | Description | Example |
|-----------------|-------------|---------|
| `<repository>` | Remote name to pull from. When omitted, uses the current branch's configured upstream. | `libra pull origin` |
| `<refspec>` | Branch name on the remote. Requires `<repository>`. When omitted, uses the current branch name. | `libra pull origin main` |
| `--ff-only` | Refuse to create a merge commit; permits only fast-forward or already-up-to-date integration; unresolved index entries still refuse the merge phase. Conflicts with `--rebase`, `--ff`, and `--no-ff`. | `libra pull --ff-only` |
| `--ff` | Explicitly allow a fast-forward merge, overriding `pull.ff=false|only`. Conflicts with `--no-ff`, `--ff-only`, and `--rebase`. | `libra pull --ff` |
| `--no-ff` | Always create a merge commit even when a fast-forward is possible. Conflicts with `--ff`, `--ff-only`, and `--rebase`. | `libra pull --no-ff` |
| `--squash` | Stage the merged tree without committing, moving `HEAD`, or recording merge state, including on conflict. Resolve and stage conflicts, then make a plain single-parent `libra commit`; merge control actions are unavailable. Conflicts with `--no-commit`, `--rebase`. | `libra pull --squash` |
| `--no-commit` | Merge and stage but stop before committing, recording merge state to finalize with `libra merge --continue`. Conflicts with `--squash`, `--rebase`. | `libra pull --no-commit` |
| `--commit` | Commit the merge result; last-one-wins with `--no-commit` and does not override fast-forward policy. Conflicts with `--squash` and `--rebase`. | `libra pull --commit` |
| `--autostash` | Stash tracked changes before integrating and re-apply them when the integration concludes (rebase: even on failure; merge: held across a non-squash conflict until `merge --continue`/`--abort`; conflicted squash: saved directly in `stash list` for `stash pop` after resolution and commit), so `pull` works on a dirty tree. Untracked/ignored files are left in place. | `libra pull --autostash` |
| `--no-progress` | Suppress the fetch progress meter (the "Receiving objects" spinner), matching `git pull --no-progress`. | `libra pull --no-progress` |
| `--notes` | Forward to the fetch: also import the file-dependency graph (`refs/notes/deps`, lore.md 3.2) from a **local Libra** upstream. Default OFF (Git parity); a network/plain-Git upstream warns and imports nothing (deferred, D17). See `libra fetch --notes`. | `libra pull --notes` |
| `--depth <n>` | Limit the fetch phase to a shallow history of `n` commits per tip. Conflicts with `--rebase`. Local Libra upstreams fail closed with `LBR-REPO-002` because they cannot advertise shallow boundaries (accepted end state, decision D20). | `libra pull --depth 1` |
| `-r`, `--rebase` | After fetching, rebase the current branch onto the upstream tip instead of merging. | `libra pull --rebase` |
| `--no-rebase` | Merge instead of rebasing, countermanding an earlier `--rebase`/`-r` and overriding `pull.rebase` for this invocation. | `libra pull --no-rebase` |
| `--allow-unrelated-histories` | Allow merging histories that have no common ancestor (Git parity), forwarded to the merge phase. Without it, pulling unrelated histories is refused with a hint pointing at this flag. | `libra pull --allow-unrelated-histories origin main` |
| `--json` | Emit structured JSON envelope to stdout (global flag). | `libra pull --json` |
| `--machine` | Compact single-line JSON; suppresses progress (global flag). | `libra pull --machine` |
| `--quiet` | Suppress all progress and merge summary output. | `libra pull --quiet` |

## Repository hooks

The integration phase uses the same `.libra/hooks` lifecycle as its selected
operation. Merge mode runs the merge hooks, including message hooks for an
automatic merge commit. Rebase mode runs blocking `pre-rebase <upstream>`
after fetch but before local history moves, then advisory `post-rewrite rebase`
after a successful rewrite. Pull has no dedicated `--no-verify`; set
`LIBRA_NO_HOOKS=1` only after reviewing the policy impact. Quiet, JSON, and
machine output suppress nested hook stdout/stderr. See
[Repository hooks](repository-hooks.md).

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
- Exactly one of `merge` or `rebase` is present, depending on the effective integration mode selected by CLI flags and `pull.rebase`/`branch.<name>.rebase` defaults. Therefore a configured `pull.rebase=true` can produce the `rebase` object even when the command did not include `--rebase`.
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
| Rebase config defaults | `branch.<name>.rebase` overrides `pull.rebase` when no CLI rebase flag is present | Same | N/A |
| Force merge commit | `libra pull --no-ff` | `git pull --no-ff` | N/A |
| Fast-forward config defaults | `pull.ff=true|false|only` when no CLI fast-forward flag is present | Same | N/A |
| Shallow pull | `libra pull --depth 1` | `git pull --depth 1` | N/A; requires a Git shallow-capable upstream, local Libra upstreams fail closed |
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
| Configured local upstream (`branch.<name>.remote=.`) | `LBR-CLI-003` | 129 | "use 'libra branch --unset-upstream' to clear the local upstream"; network support is issues/480 HP-16 |
| Invalid `pull.rebase`, `branch.<name>.rebase`, or `pull.ff` config value | `LBR-CLI-002` | 129 | "libra config <key> <value>" |
| Unsupported `pull.rebase=merges|interactive` mode | `LBR-CLI-002` | 129 | Use boolean rebase or an explicit supported pull flag |
| Invalid `merge.renames` / `merge.renameLimit` / `merge.directoryRenames` / `merge.renormalize` (inherited from `libra merge`) | `LBR-REPO-003` | 128 | Set the named merge key to a supported value or remove it |
| Fetch: network unreachable / timeout | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| Fetch: packet-read connection reset / non-protocol IO failure | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| Fetch: authentication failed | `LBR-AUTH-001` | 128 | "check SSH key or HTTP credentials" |
| Fetch: pkt-line discovery / transfer setup error | `LBR-NET-002` | 128 | "check that the remote serves Git data and that a proxy has not altered the response" |
| Fetch: pkt-line truncation / sideband / checksum / pack protocol error | `LBR-NET-002` | 128 | No additional hint, except for an incomplete pack: "the connection dropped mid-transfer — retry the pull" |
| Merge: non-squash conflicts, dirty worktree, or untracked overwrite | `LBR-CONFLICT-002` | 128 | "resolve conflicts, then run 'libra merge --continue'" |
| Merge: unresolved index without merge state, or a new squash conflict | `LBR-CONFLICT-002` | 128 | "resolve conflicts, stage the resolved paths with 'libra add', then run 'libra commit'" |
| Merge: non-fast-forward rejected by `--ff-only` | `LBR-CONFLICT-002` | 128 | "run 'libra pull' without --ff-only to allow a merge commit" |
| Merge or rebase would have to arbitrate a `160000` gitlink (submodule) | `LBR-UNSUPPORTED-001` | 128 | Resolve the submodule pointer outside Libra, or drop the gitlink entry — refused before the autostash and before any index/worktree write (see `docs/commands/merge.md`) |
| Merge: recursive virtual ancestor would nest deeper than 20 levels or fold more than 32 bases at one level | `LBR-UNSUPPORTED-001` | 128 | Merge the branches' common ancestors together first, or pull with `--rebase` (see `docs/commands/merge.md`) |
| Rebase: conflict during replay | `LBR-CONFLICT-001` | 128 | "resolve conflicts, stage them, then run 'libra rebase --continue'" |
| Rebase: dirty worktree | `LBR-REPO-003` | 128 | "commit or stash your changes before rebasing" |
| Merge: invalid target | `LBR-CLI-003` | 129 | "verify the upstream ref and try again" |
| Merge: unrelated histories or invalid merge state | `LBR-REPO-003` | 128 | "inspect branch history and merge state" |
| Merge: repository corruption | `LBR-REPO-002` | 128 | "inspect repository state and object integrity" |
| Merge: read failure | `LBR-IO-001` | 128 | "check repository metadata and permissions" |
| Merge: write failure | `LBR-IO-002` | 128 | "check filesystem permissions and retry" |

### Phase Detail

When a sub-operation fails, the error JSON includes a `phase` key in the details object (`"fetch"`, `"merge"`, or `"rebase"`) so agents can distinguish which stage failed.

## Malformed HTTP(S) discovery responses

During HTTP(S) reference discovery, Libra rejects a zero-byte advertisement and
malformed pkt-line frames, including short or non-hexadecimal headers, frame
lengths below four, and truncated payloads. A valid `0000` flush remains distinct
from an absent response; a valid empty-repository advertisement is supported.
An unsupported object-format capability reports the fixed message
`Unsupported object format capability` without echoing its remote value.
Check that the URL points to a Git smart HTTP service and that a proxy has not
truncated or replaced the response; then retry.

## pkt-line error classification

Detected pkt-line framing errors return `LBR-NET-002` (exit 128), including an
empty HTTP(S) discovery advertisement. Ordinary connection failures, resets and
timeouts return `LBR-NET-001` (exit 128). Verify the Git service and any proxy
response when a protocol error occurs. Discovery framing errors use the hint
`check that the remote serves Git data and that a proxy has not altered the response`.

The fetch phase uses this classification for discovery and object-transfer
setup. A truncated header or payload while reading the fetch stream returns
`LBR-NET-002` with no extra CLI hint; an incomplete pack at a clean frame boundary
retains its byte count and `the connection dropped mid-transfer — retry the pull`
hint. A packet-read connection reset is `LBR-NET-001`. These errors keep
`details.phase = "fetch"` in JSON output.

An upload-pack EOF at a frame boundary before pack data begins, including a
zero-byte POST response, returns `LBR-NET-001` with
`check network connectivity and retry`. An empty discovery advertisement remains
`LBR-NET-002`.

## Git and SSH advertisement frame boundaries

The Git/SSH pkt-line advertisement readers reject declared lengths `0001` through
`0003`, incomplete four-byte headers, and EOF inside a declared payload. Flush
`0000`, empty-payload `0004` and maximum-size `ffff` frames retain their behavior.
Their typed pkt-line errors classify as `LBR-NET-002` at the reader boundary;
ordinary transport IO and idle timeouts remain `LBR-NET-001` when classified.

During the `git://` object-fetch advertisement, fetch, clone and pull already
report these failures as `LBR-NET-002`, including a zero-byte advertisement. The
hint is `check that the remote serves Git data and that a proxy has not altered the response`.
Lengths 1–3 previously could panic; truncated advertisements previously returned
`LBR-NET-001` with a network/transfer hint. Git discovery now preserves these protocol errors through the command boundary.
SSH propagation and bounded cleanup are described below. All readers require
four ASCII hexadecimal header digits.

This advertisement is distinct from an upload-pack response after negotiation:
HTTP(S) discovery framing and empty upload-pack response classifications are unchanged.
Reader tests exercise local TCP object-fetch advertisements and public error
conversions. Separate command tests exercise malformed Git discovery and SSH
cleanup. Check the remote Git service or proxy for malformed frames.

## SSH advertisement error handling

SSH advertisement lengths `0001` through `0003`, incomplete headers (including
zero-byte EOF), and truncated payloads return `LBR-NET-002`. The fixed protocol
reason and marker are retained without captured SSH stdout/stderr.

An incomplete required header has one host-trust exception: local SSH exit status
255 together with a recognized host-key diagnostic in the first 64 KiB of stderr
returns fixed host-verification guidance and `LBR-NET-001`. This classification
does not verify the remote fingerprint. Other missing advertisements, including
authentication failures, still use `LBR-NET-002`; an available non-zero local exit
status adds `SSH exited with status N` and fixed connectivity, trusted-host,
ssh-agent and repository-access guidance. Original SSH diagnostic text is hidden.

After an incomplete required header, Libra allows up to 100 milliseconds to
observe the SSH exit status, then requests termination if needed. Other read
errors request termination immediately. The status window, direct-child reap and
output collection share a two-second cleanup deadline. Protocol and typed
host-trust errors take precedence over secondary cleanup warnings. Ordinary IO
and timeout errors keep their transport classification and may include a fixed
local cleanup warning. Termination can change the observed exit status. This
does not promise cleanup of arbitrary descendant processes.

Clone places targeted host-verification guidance in its structured hints. The
other command boundaries retain fixed host guidance in the message and their
existing `LBR-NET-001` network hint. Human, JSON and machine diagnostics omit raw
captured remote stderr in either case.

The `git://` discovery and object-fetch paths preserve the listed frame errors as
`LBR-NET-002`. All asynchronous readers reject non-ASCII/non-hexadecimal headers
with fixed protocol reasons. HTTP(S) discovery/advertisement framing is unchanged.

## SSH authentication and captured diagnostics

Libra invokes SSH with `BatchMode=yes` for both terminal and non-terminal callers.
It does not prompt for a private-key passphrase or an interactive host-key
decision during a Libra command. Load or unlock an encrypted key in `ssh-agent`
before retrying. For host trust, verify the fingerprint through a trusted
provider console or another trusted channel before manually updating
`~/.ssh/known_hosts`. Alternatively, make a separate interactive SSH connection
and compare the displayed fingerprint before accepting it. For example,
`ssh -T git@github.com` uses GitHub; use the actual repository SSH user, host and
port. Do not accept a fingerprint that has not been verified.

`ssh.strictHostKeyChecking` retains its existing `ask`, `yes`, `accept-new` and
`no` values. `ask` leaves that SSH option to the user's SSH configuration;
`BatchMode=yes` still prevents interactive decisions. Explicit values are
forwarded to SSH. Choose a host-trust policy appropriate to your repository.

SSH stderr is always captured, including in terminal sessions. It is drained
from process startup, retaining at most 64 KiB while counting and hashing the
remaining bytes. User-facing errors contain fixed text and a local exit status
when available. Raw remote stderr is neither printed nor logged. Debug diagnostics
contain only the status, total and retained byte counts, and a SHA-256 digest of
the collected stream. Failed or cancelled collection may prevent these metadata
from being reported; no completed digest is claimed in that case. Hashing work
is proportional to the number of bytes drained.

SSH reference advertisements and receive-pack responses each have a 16 MiB
aggregate limit. An oversized advertisement fails with `LBR-NET-001` and guidance
to use the repository’s HTTPS URL if available, or ask its maintainer to reduce refs. An oversized push response fails with `LBR-NET-001`
and guidance to push fewer refs; it is not accepted as a truncated success.
These limits can affect repositories with very large ref sets or updates. The
streamed fetch pack is not subject to this cap. A failed push response does not
prove that the server rolled back its refs: inspect the remote state before
retrying. Existing IO timeouts still apply.

After a complete discovery advertisement, Libra allows up to 100 milliseconds
for SSH to exit before requesting termination, within a two-second total
cleanup deadline. Captured-output tasks are cancelled when their owner exits or
their deadline expires, including when a descendant keeps a pipe open.

### SSH host identity and diagnostic collection

SSH host identity changes retain a distinct fixed warning: the change may
indicate interception or legitimate key rotation. Verify the new fingerprint
through a trusted channel before replacing an existing known_hosts entry; do not
bypass host-key checking. Unknown and changed host keys both use LBR-NET-001,
but their fixed messages and guidance differ.

A stderr collection timeout does not by itself discard complete protocol output
and an observed local exit status. Non-zero exit status and primary read errors
still fail the operation. Unavailable diagnostics produce only a fixed debug
notice, without fabricated empty-stream counts or digests. Stdout collection or
process-wait failures retain their normal error handling.

### SSH limits and host-classification boundaries

These fixed 16 MiB advertisement and receive-pack response limits apply only to
Libra's SSH transport. The HTTPS and Git transports do not impose this particular
cap. If the server provides an HTTPS endpoint, use its HTTPS remote URL when an
SSH advertisement exceeds the cap; this does not require a read-only user to
change the server's refs. Otherwise, ask the repository maintainer to reduce the
advertised ref set. The streamed fetch pack remains outside this aggregate cap.

Host-trust classification requires an incomplete first header with no stdout
bytes observed, local exit 255 and a recognized retained stderr pattern. Once
any stdout byte arrives, including a partial header, host-like stderr cannot
select host-specific guidance. Failures after a complete advertisement retain
fixed generic diagnostics. The pre-advertisement pattern remains a diagnostic
heuristic, not fingerprint verification.

A successful discovery whose child waits for a request normally incurs the full
100 ms native-exit observation window, once per discovery operation. This is
separate from the two-second direct-child cleanup budget; no benchmark or
arbitrary-descendant cleanup guarantee is implied.

## Strict pkt-line headers

A pkt-line header must contain exactly four ASCII hexadecimal digits (`0`–`9`,
`a`–`f` or `A`–`F`). Fetch streaming, `git://` advertisements and SSH advertisements
reject leading signs such as `+004`, whitespace, non-hexadecimal text and invalid
UTF-8. These failures return `LBR-NET-002` (exit 128), with fixed reasons that do
not echo the header or payload. A peer that previously sent a signed or otherwise
nonconforming header must send four hexadecimal digits before retrying.

Git discovery also preserves protocol classification for lengths `0001`–`0003`,
missing or partial required headers and truncated payloads. The same discovery
classification reaches clone, fetch, pull, ls-remote and push. Check the remote
Git service or proxy response. Their existing structured error fields remain;
push retains its own protocol hint and the other commands retain theirs.

Flush `0000`, empty-data `0004` and maximum-length `ffff` frames keep their existing
meaning. Ordinary network errors and timeouts retain their existing categories.
An empty fetch data stream before any complete pack remains a network failure;
EOF after a completed pack keeps the existing success behavior. The SSH host-trust
exception, captured-diagnostic limits and cleanup deadlines described above remain.

## Empty-repository discovery framing

An HTTP(S) advertisement that declares an empty repository still has all remaining
pkt-line frames checked. A malformed header, an unsupported length 1..3, or a truncated
payload after the zero object ID returns `LBR-NET-002` (exit 128), with a fixed
reason that does not echo the remote bytes. It is no longer reported as a
successful empty response. Check the remote Git service or proxy response before
retrying. Valid empty repositories, supported SHA-1/SHA-256 advertisements,
existing command hints and structured error fields retain their behavior.

## Issue #477 notes

refuses a local upstream (`branch.<name>.remote=.`)
