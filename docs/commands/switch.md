# `libra switch`

Switch branches, create and switch to a new branch, or detach HEAD at a specific commit.

**Alias:** `sw`

## Synopsis

```
libra switch <branch>
libra switch -
libra switch -c <name> [<start-point>]
libra switch -C <name> [<start-point>]
libra switch --orphan <name>
libra switch -d <commit|tag|branch>
libra switch --track <remote/branch>
libra switch [--guess | --no-guess] <branch>
```

## Description

`libra switch` is the primary command for changing branches. It validates that the working tree is clean before switching, updates HEAD and the index, and restores the working tree to match the target commit. Unlike `libra checkout`, which exists as a Git-compatibility surface, `switch` is the recommended command for branch operations.

The command supports multiple modes: switching to an existing local branch (default), returning to the previous checkout target with `-`, creating a new branch with `-c`, force-creating or resetting a branch with `-C`, creating an unborn orphan branch with `--orphan`, detaching HEAD with `-d`, and tracking a remote branch with `--track`. When the target branch is already the current branch, the command is a no-op and skips the cleanliness check entirely.

`libra switch -` selects the source of the most recent `switch` or `checkout` movement in this worktree's HEAD reflog. A branch source follows that local branch's current tip; a detached source uses the full stored object ID. Every successful movement is recorded, so repeating `switch -` toggles between the two targets. If there is no navigation record, the recorded branch was deleted, or the newest record is malformed, Libra fails before changing HEAD, the index, or the working tree.

Fuzzy branch name suggestions are provided via Levenshtein distance when a branch is not found, helping catch typos without requiring exact matches.

After a state-changing switch, advisory `.libra/hooks/post-checkout` receives the old
OID, new OID, and branch flag `1`. Already-on no-ops do not invoke it. Set
`LIBRA_NO_HOOKS=1` only for an explicit policy bypass. See
[Repository hooks](repository-hooks.md) for sandbox and output behavior.

## Options

| Flag | Long | Value | Description |
|------|------|-------|-------------|
| | `<branch>` | positional (optional) | Target local branch, `-` for the previous checkout target, or a commit/tag/branch with `--detach` |
| `-c` | `--create` | `<name>` | Create a new branch and switch to it |
| `-C` | `--force-create` | `<name>` | Create a new branch or reset an existing one and switch to it |
| | `--orphan` | `<name>` | Create a new unborn orphan branch with no parents and switch to it |
| `-d` | `--detach` | | Detach HEAD at the given commit, tag, or branch |
| | `--track` | | Create a local branch tracking the given remote branch and switch to it |
| | `--guess` | | Auto-create a tracking branch when `<branch>` uniquely matches one remote (default; DWIM) |
| | `--no-guess` | | Disable the remote-tracking guess; require a local branch or explicit `--track` |
| | `--ignore-other-worktrees` | | Accepted for CLI compatibility, but **does NOT** bypass Libra's other-worktree safety guard (intentionally-different from Git): Libra never allows the same shared branch checked out in two worktrees. It is a silent no-op in a single-worktree repo; against a real collision the switch is still refused, and the refusal points at `libra worktree doctor` / `libra worktree repair --confirm` for settling a stale recorded owner. |
| | `--no-progress` | | Do not show a progress meter. Accepted no-op for Git parity: Libra's switch never renders a progress meter. |

### Flag details

**`-c / --create <name> [start-point]`**: Creates a new branch named `<name>` from `<start-point>` (or HEAD if omitted), then switches to it. Validates the name, checks that no branch with that name already exists, and rejects reserved internal branch names.

```bash
libra switch -c feature-x              # New branch from HEAD
libra switch -c fix-123 abc1234        # New branch from specific commit
libra switch -c release-2.0 main       # New branch from another branch
```

**`-C / --force-create <name> [start-point]`**: Like `--create`, but if a branch named `<name>` already exists it is deleted and recreated from `<start-point>` (or HEAD). Refuses to delete the currently checked-out branch.

```bash
libra switch -C feature-x              # Reset feature-x to HEAD and switch
libra switch -C fix-123 abc1234        # Reset fix-123 to specific commit
```

After `-c` or `-C` succeeds, `HEAD` remains a symbolic reference to the created/reset branch (`refs/heads/<name>`), including when a start-point was provided.

**`--orphan <name>`**: Creates an unborn branch with no parent history, then switches `HEAD` to `refs/heads/<name>` without creating a branch ref yet. The index and working tree are preserved from the previous branch, so the first user commit on the orphan branch records the retained index as a root commit with no parents. The working tree must be clean before the switch, and an existing branch name is rejected fail-closed.

```bash
libra switch --orphan fresh-start      # Unborn branch; first commit has no parents
```

**`-d / --detach`**: Moves HEAD to point directly at a commit rather than a branch. Useful for inspecting historical states or building from tags.

```bash
libra switch --detach v1.0             # Detach at a tag
libra switch --detach abc1234          # Detach at a commit
```

**`-`**: Returns to the previous branch or detached commit recorded by the latest HEAD navigation entry. The shortcut is shared with `checkout -`, so either command can toggle a movement made by the other.

```bash
libra switch topic
libra switch -                         # Return to the prior branch
libra switch -                         # Return to topic
```

**`--track`**: Looks up the remote-tracking reference, creates a local branch with the same name, sets upstream tracking, and switches to it. Conflicts with `--create` and `--detach`.

```bash
libra switch --track origin/main       # Track and switch to remote branch
libra switch --track feature            # Assumes origin/feature
```

**`--guess` / `--no-guess`**: When `<branch>` is not an existing local branch but exactly one remote has a tracking branch of that name, `--guess` (the default) creates a local branch of the same name, sets it to track that remote branch, and switches to it -- the same single-step behavior as `--track <remote>/<branch>`. Guessing is on by default; the effective setting follows `--no-guess` > `--guess` > `checkout.guess` (default `true`), so an explicit flag always overrides the config. When several remotes carry the name, the switch fails with an ambiguity error (exit 128) unless `checkout.defaultRemote` selects one. The explicit `remote/branch` form (e.g. `libra switch origin/main`) is unaffected and still errors with a hint to use `--track`.

```bash
libra switch feature                   # Auto-track origin/feature if only origin has it
libra switch --no-guess feature        # Fail instead of guessing a remote branch
```

## Common Commands

```bash
libra switch main                      # Switch to an existing branch
libra switch -                         # Return to the previous checkout target
libra switch -c feature-x              # Create and switch to a new branch
libra switch -c fix-123 abc1234        # Create branch from specific commit
libra switch -C feature-x              # Reset branch to HEAD and switch
libra switch --orphan fresh-start      # Create unborn branch; first commit has no parents
libra switch --detach v1.0             # Detach HEAD at a tag
libra switch --track origin/main       # Track and switch to remote branch
libra switch feature                   # Auto-create a tracking branch from a unique remote (guess)
libra switch --no-guess feature        # Disable remote-tracking guessing
libra switch --json main               # Structured JSON output for agents
```

## Human Output

Default human mode writes the result to `stdout`.

Switch to an existing branch:

```text
Switched to branch 'main'
```

Create and switch to a new branch:

```text
Switched to a new branch 'feature'
```

Detach HEAD at a commit:

```text
HEAD is now at abc1234
```

Already on the target branch (no-op):

```text
Already on 'main'
```

`--quiet` suppresses all `stdout` output.

## Structured Output (JSON examples)

`libra switch` supports the global `--json` and `--machine` flags.

- `--json` writes one success envelope to `stdout`
- `--machine` writes the same schema as compact single-line JSON
- `stderr` stays clean on success

Switch to an existing branch:

```json
{
  "ok": true,
  "command": "switch",
  "data": {
    "previous_branch": "main",
    "previous_commit": "abc1234def5678901234567890abcdef12345678",
    "branch": "feature",
    "commit": "def5678abc1234901234567890abcdef12345678",
    "created": false,
    "detached": false,
    "unborn": false,
    "already_on": false,
    "tracking": null
  }
}
```

Create and switch to a new branch:

```json
{
  "ok": true,
  "command": "switch",
  "data": {
    "previous_branch": "main",
    "previous_commit": "abc1234def5678901234567890abcdef12345678",
    "branch": "feature-x",
    "commit": "abc1234def5678901234567890abcdef12345678",
    "created": true,
    "detached": false,
    "unborn": false,
    "already_on": false,
    "tracking": null
  }
}
```

Create an unborn orphan branch:

```json
{
  "ok": true,
  "command": "switch",
  "data": {
    "previous_branch": "main",
    "previous_commit": "abc1234def5678901234567890abcdef12345678",
    "branch": "fresh-start",
    "commit": "0000000000000000000000000000000000000000",
    "created": true,
    "detached": false,
    "unborn": true,
    "already_on": false,
    "tracking": null
  }
}
```

Detach HEAD at a tag or commit:

```json
{
  "ok": true,
  "command": "switch",
  "data": {
    "previous_branch": "main",
    "previous_commit": "abc1234def5678901234567890abcdef12345678",
    "branch": null,
    "commit": "def5678abc1234901234567890abcdef12345678",
    "created": false,
    "detached": true,
    "unborn": false,
    "already_on": false,
    "tracking": null
  }
}
```

Track and switch to a remote branch:

```json
{
  "ok": true,
  "command": "switch",
  "data": {
    "previous_branch": "main",
    "previous_commit": "abc1234def5678901234567890abcdef12345678",
    "branch": "feature",
    "commit": "def5678abc1234901234567890abcdef12345678",
    "created": true,
    "detached": false,
    "unborn": false,
    "already_on": false,
    "tracking": {
      "remote": "origin",
      "remote_branch": "feature"
    }
  }
}
```

### Schema Notes

- `previous_branch` is `null` when HEAD was detached before the switch
- `branch` is `null` when HEAD is now detached (`--detach`)
- `unborn` is `true` only after `--orphan`; `commit` is the all-zero OID until the first user commit creates the branch ref
- `already_on` is `true` when the target branch equals the current branch (no-op)
- `tracking` is present with `--track` or a successful guess, containing `remote` and `remote_branch`
- `created` is `true` when `--create`, `--force-create`, `--track`, or a guess created or reset a local branch

## Design Rationale

### Why separate from checkout?

Git's `checkout` is overloaded: it switches branches, restores files, detaches HEAD, and creates branches -- all through the same command with different flag combinations. This makes it difficult for both humans and AI agents to predict behavior. Libra follows Git's own modernization path (introduced in Git 2.23) by splitting `checkout` into `switch` (branch operations) and `restore` (file operations). `libra switch` handles only branch-related state changes, making its behavior predictable and its error messages precise.

Keeping `switch` focused also simplifies structured output: every `SwitchOutput` contains the same fields regardless of the operation mode, so agents can parse results without guessing which schema variant applies.

### Why auto-track remote branches?

When `--track origin/feature` is used, Libra automatically creates a local branch, sets upstream tracking, and switches to it in a single atomic operation. This eliminates the multi-step ceremony of `git fetch && git branch feature origin/feature && git branch -u origin/feature feature && git switch feature`. For AI agents operating in trunk-based workflows, reducing a four-command sequence to one command means fewer failure points and simpler tool orchestration.

The `--track` flag defaults to the `origin` remote when only a branch name is provided (e.g., `libra switch --track feature` assumes `origin/feature`), which matches the most common remote setup.

### Why fuzzy suggestions?

When a branch name is not found, Libra computes Levenshtein distance against all known branches and suggests matches within edit distance 2. This catches common typos (`faeture` instead of `feature`) without requiring glob patterns or regex. The suggestions appear as actionable hints in the error output, reducing round-trips for both human users and AI agents that can parse hint text.

## Parameter Comparison: Libra vs Git vs jj

| Feature | Git | Libra | jj |
|---------|-----|-------|----|
| Switch branch | `git switch main` | `libra switch main` | `jj edit <rev>` |
| Previous checkout target | `git switch -` | `libra switch -` | N/A |
| Create and switch | `git switch -c feature` | `libra switch -c feature` | `jj new -m "feature"` + `jj branch create feature` |
| Create from commit | `git switch -c fix abc1234` | `libra switch -c fix abc1234` | `jj new abc1234` + `jj branch create fix` |
| Detach HEAD | `git switch --detach v1.0` | `libra switch --detach v1.0` | `jj edit <rev>` (always detached-like) |
| Track remote | `git switch --track origin/main` | `libra switch --track origin/main` | N/A (jj tracks all remotes) |
| Force create | `git switch -C feature` | `libra switch -C feature` | N/A |
| Orphan branch | `git switch --orphan <name>` | `libra switch --orphan <name>` | `jj new root()` |
| Structured output | No | `--json` / `--machine` | `--template` |
| Fuzzy suggestions | No | Levenshtein-based "did you mean" hints | No |
| Clean-state validation | Warns but proceeds (sometimes) | Blocks switch with actionable error | No dirty state concept |

## Error Handling

Every `SwitchError` variant maps to an explicit `StableErrorCode`.

| Scenario | Error Code | Exit | Hint |
|----------|-----------|------|------|
| Missing track target | `LBR-CLI-002` | 129 | "provide a remote branch name, for example 'origin/main'." |
| Missing detach target | `LBR-CLI-002` | 129 | "provide a commit, tag, or branch to detach at." |
| Missing branch name | `LBR-CLI-002` | 129 | "provide a branch name." |
| Branch not found | `LBR-CLI-003` | 129 | "create it with 'libra switch -c {name}'." + fuzzy suggestions |
| No resolvable previous checkout target | `LBR-CLI-003` | 129 | "switch to another branch or commit first." |
| Previous checkout reflog read failed | `LBR-IO-001` | 128 | Check `.libra/libra.db` permissions and retry. |
| Previous checkout record is malformed or unreadable | `LBR-REPO-002` | 128 | Inspect `libra reflog show HEAD` and remove or repair the newest corrupt navigation entry. |
| Got remote branch | `LBR-CLI-003` | 129 | "use 'libra switch --track ...' to create a local tracking branch." |
| Remote branch not found | `LBR-CLI-003` | 129 | "Run 'libra fetch {remote}' to update remote-tracking branches." |
| Invalid remote branch | `LBR-CLI-003` | 129 | "expected format: 'remote/branch'." |
| Ambiguous guess remote | `LBR-CONFLICT-002` | 128 | "it exists on remotes: ..." + "use 'libra switch --track <remote>/<branch>' to pick one, or set checkout.defaultRemote." |
| Branch already exists | `LBR-CONFLICT-002` | 128 | "use 'libra switch {name}' if you meant the existing local branch." |
| Internal branch blocked | `LBR-CLI-003` | 129 | -- |
| Unstaged changes | `LBR-REPO-003` | 128 | "commit or stash your changes before switching." |
| Uncommitted changes | `LBR-REPO-003` | 128 | "commit or stash your changes before switching." |
| Untracked file would be overwritten | `LBR-CONFLICT-002` | 128 | "move or remove it before switching." |
| Status check failed | `LBR-IO-001` | 128 | -- |
| Commit resolve failed | `LBR-CLI-003` | 129 | "check the revision name and try again." |
| Branch creation failed | `LBR-IO-002` | 128 | -- |
| HEAD update failed | `LBR-IO-002` | 128 | -- |
| Delegated (branch/restore) | Original code | Original | Original hints |

`switch -c <existing-branch>` currently preserves the original `branch`
command conflict contract through `DelegatedCli`, so that path keeps the branch
command's existing error shape instead of adding the `SwitchError::BranchAlreadyExists`
hint.
