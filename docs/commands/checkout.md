# `libra checkout`

Show the current branch, switch to an existing branch, create and switch to a new branch, or restore paths through the explicit `--` compatibility form.
Compatible with `git checkout` for common branch operations and explicit path restoration.

## Synopsis

```
libra checkout [<branch>]
libra checkout -b <name> [<start-point>]
libra checkout -B <name> [<start-point>]
libra checkout --orphan <name>
libra checkout [<tree-ish>] -- <pathspec>...
```

## Description

`libra checkout` is a Git-compatibility surface that delegates to `switch` and `restore` internally. It supports the most common `git checkout` patterns: showing the current branch, switching to an existing branch, creating a new branch with `-b` from HEAD or an explicit start-point, force-creating or resetting a branch with `-B` from HEAD or an explicit start-point, creating an unborn orphan branch with `--orphan`, checking out a commit to enter detached HEAD, auto-tracking remote branches, and restoring paths when an explicit `--` separator is present.

This command exists so that developers migrating from Git can use familiar muscle memory. For new workflows, prefer `libra switch` (for branch operations) and `libra restore` (for file operations), which provide richer error messages, structured JSON output, and clearer semantics.

When checking out a branch name that does not exist locally but matches a remote-tracking branch (e.g., `origin/feature`), Libra automatically creates a local tracking branch, sets upstream, and pulls -- going further than Git's auto-track by also synchronizing content immediately.

Path restoration is only enabled by an explicit `--` separator. Without `--`, `libra checkout <name>` is always branch mode, even when a file has the same name. The pathspecs after `--` use the same shared Git-style matcher as `libra restore`: plain prefixes, wildcard pathspecs, and `:(top)`/`:/`/`:(glob)`/`:(literal)`/`:(icase)`/`:(exclude)`/`:!`/`:^` magic are honored. Wildcard-looking pathspecs also match an exact path or directory prefix with the same literal text.

When path restoration materializes a tracked symlink, Libra writes a real
symlink on Unix using the stored link target bytes. Platforms without symlink
support return an explicit unsupported-symlink diagnostic instead of silently
writing a regular file that contains the target path.

## Options

| Flag | Long | Value | Description |
|------|------|-------|-------------|
| | `<branch>` | positional (optional) | Target branch to switch to. Omit to show current branch. |
| `-b` | | `<name>` | Create a new branch from `[<start-point>]` or the current HEAD and switch to it |
| `-B` | | `<name>` | Force-create a branch from `[<start-point>]` or the current HEAD and switch to it; resets an existing branch to that commit |
| | `[<start-point>]` | positional | Optional commit, tag, or branch used with `-b` / `-B` as the new branch tip |
| | `--orphan` | `<name>` | Create an unborn orphan branch, preserve the index/worktree, and switch HEAD to it. A separate start-point is not supported. |
| `-d` | `--detach` | | Detach HEAD at the named commit even when it is a branch (instead of switching to the branch) |
| `-t` | `--track` | | Set up upstream tracking when checking out a remote-tracking branch. Accepted as a no-op: Libra always configures tracking for a remote-tracking checkout (DWIM), so this requests behavior Libra already performs; no effect for a non-remote target. Use `libra switch --track` for explicit, standalone tracking. |
| | `--ignore-other-worktrees` | | Check out a branch even if another linked worktree has that shared branch checked out; bypasses Libra's other-worktree safety guard. |
| | `--no-progress` | | Do not show a progress meter. Accepted as a no-op: Libra's checkout never renders a progress meter. |
| | `--no-overlay` | | Do not check out paths in overlay mode (paths missing from the source are still removed). Accepted as a no-op: Libra's checkout is never in overlay mode, matching the Git default. (Git's `--overlay` is not implemented.) |
| | `[<tree-ish>] -- <pathspec>...` | positional | Restore paths with shared pathspec magic. Without `<tree-ish>`, restores the worktree from the index. With `<tree-ish>`, restores both index and worktree from that source. |

### Flag examples

```bash
# Show the current branch
libra checkout

# Switch to an existing local branch
libra checkout main

# Create and switch to a new branch
libra checkout -b feature-x
libra checkout -b fix-123 abc1234

# Force-create (or reset) and switch to a branch at the current HEAD or start-point
libra checkout -B feature-x
libra checkout -B feature-x main

# Create an unborn orphan branch; first commit has no parents
libra checkout --orphan fresh-start

# Auto-track a remote branch (creates local, sets upstream, pulls)
libra checkout feature

# Restore a path from the index to the worktree
libra checkout -- src/main.rs

# Restore a path from HEAD to both index and worktree
libra checkout HEAD -- src/main.rs

# Restore Rust files except generated output
libra checkout -- ':(glob)src/*.rs' ':(exclude)src/generated.rs'

# Restore a tracked symlink as a symlink
libra checkout HEAD -- link-to-target
```

## Common Commands

```bash
libra checkout                         # Show the current branch
libra checkout main                    # Switch to an existing local branch
libra checkout feature-x               # Switch to another branch
libra checkout -b feature-x            # Create and switch to a new branch
libra checkout -b fix-123 abc1234      # Create and switch from a start-point
libra checkout -B feature-x            # Force-create or reset and switch to a branch
libra checkout -B feature-x main       # Reset branch to a start-point and switch
libra checkout --orphan fresh-start    # Create unborn branch; first commit has no parents
libra checkout -- file.txt             # Restore file from index to worktree
libra checkout HEAD -- file.txt        # Restore file from HEAD to index + worktree
libra --json checkout main             # Structured compatibility output
libra checkout --quiet main            # Switch without informational stdout
```

## Human Output

Default human mode writes the result to `stdout`.

Show current branch:

```text
Current branch is main.
```

Show detached HEAD:

```text
HEAD detached at abc1234d
```

Switch to an existing branch:

```text
Switched to branch 'main'
```

Create and switch to a new branch:

```text
Switched to a new branch 'feature-x'
```

When `-b` or `-B` is used with a start-point, the created/reset branch becomes the active symbolic `HEAD` (`refs/heads/<branch>`); Libra does not leave the repository detached after the operation.

Create and switch to an unborn orphan branch:

```text
Switched to a new branch 'fresh-start'
```

After `checkout --orphan`, `HEAD` is a symbolic reference to `refs/heads/<branch>`, but that branch ref does not resolve until the first user commit. The index and working tree are preserved from the previous branch; the first commit has no parents. If the branch already exists, Libra rejects the command without deleting or moving it.

Auto-track a remote branch:

```text
branch 'feature' set up to track 'origin/feature'.
Switched to a new branch 'feature'
Branch 'feature' set up to track remote branch 'origin/feature'
```

Depending on the remote state, the follow-up `pull` step may emit additional
synchronization output.

Already on the target branch (no-op):

```text
Already on main
```

Path restore:

```text
Updated 1 path(s) from HEAD
```

`--quiet` suppresses all `stdout` output.

## Structured Output (JSON)

`checkout` supports `--json` and `--machine` for the compatibility surface. `--json` emits a normal command envelope; `--machine` emits the same envelope as one NDJSON line. Nested `restore`, branch-upstream, and pull output is suppressed so stdout contains only the checkout result.

Example for switching to an existing local branch:

```json
{
  "ok": true,
  "command": "checkout",
  "data": {
    "action": "switch",
    "previous_branch": "main",
    "previous_commit": "abc1234...",
    "branch": "feature-x",
    "commit": "def5678...",
    "short_commit": "def5678a",
    "switched": true,
    "created": false,
    "pulled": false,
    "already_on": false,
    "detached": false,
    "tracking": null
  }
}
```

| Action | When emitted |
|--------|--------------|
| `show-current` | `libra checkout` with no branch |
| `already-on` | Target branch is already checked out |
| `switch` | Existing local branch checkout |
| `create` | `checkout -b <branch> [<start-point>]`, `checkout -B <branch> [<start-point>]`, or `checkout --orphan <branch>` |
| `track` | Local branch is created from `origin/<branch>` and pull is attempted |
| `restore-paths` | Explicit `checkout [<tree-ish>] -- <pathspec>...` path restoration |

Remote auto-track output sets `created: true`, `pulled: true`, and includes `tracking.remote` plus `tracking.remote_branch`.

For `checkout --orphan`, `action` is `create`, `created` is `true`, `branch` is the unborn branch name, and `commit` / `short_commit` are `null` until the first user commit creates the branch ref.

For richer branch workflows, `libra switch --json ...` remains the preferred structured command. For file workflows, `libra restore --json ...` remains preferred; checkout path mode is only a Git-compatible alias.

Example for path restoration:

```json
{
  "ok": true,
  "command": "checkout",
  "data": {
    "action": "restore-paths",
    "previous_branch": "main",
    "branch": "main",
    "switched": false,
    "restore": {
      "source": "HEAD",
      "worktree": true,
      "staged": true,
      "restored_files": ["src/main.rs"],
      "deleted_files": []
    }
  }
}
```

## Design Rationale

### Why keep checkout as a compatibility command?

Git muscle memory is deeply ingrained. Developers who have used `git checkout` for years will instinctively type `libra checkout main`. Rather than forcing an immediate mental model change, Libra provides `checkout` as a thin wrapper that handles the most common patterns. This lowers the adoption barrier while the recommended `switch`/`restore` split is documented and encouraged.

The command intentionally keeps file restoration behind Git's explicit `--` separator. Plain `libra checkout <name>` remains branch mode; `libra checkout -- <path>` and `libra checkout <tree-ish> -- <path>` are compatibility aliases for the corresponding `restore` operations.

### Visible compatibility surface (post-C5)

`checkout` is exposed in top-level help (`libra --help`) as a compatibility
surface — it is **no longer hidden**. New users coming from Git can find it
without surprise, but the help banner and the command index both steer
day-to-day usage to `switch` (branch navigation) and `restore` (file
restoration). `switch` and `restore` provide:

- Typed command-specific error enums and stable error codes
- Structured JSON output (`--json` / `--machine`)
- Fuzzy branch suggestions on typos
- Explicit semantics (no ambiguity between "switch branch" and "restore file")

### Why auto-pull on remote branch?

When `libra checkout feature` finds `origin/feature` but no local `feature` branch, it creates the local branch, sets upstream tracking, and immediately pulls. This goes beyond Git's behavior (which only creates the tracking branch without pulling). The rationale:

- **Trunk-based development**: in Libra's target workflow, checking out a remote branch implies intent to work on it, so having the latest content is almost always desired.
- **Fewer commands for agents**: an AI agent checking out a remote branch wants working content immediately, not an empty tracking branch that requires a separate `pull`.
- **Fail-fast**: if the pull fails (network error, merge conflict), the user learns immediately rather than discovering stale content later.

## Parameter Comparison: Libra vs Git vs jj

| Feature | Git | Libra | jj |
|---------|-----|-------|----|
| Show current branch | `git branch --show-current` | `libra checkout` (no args) | `jj log -r @` |
| Switch branch | `git checkout main` | `libra checkout main` | `jj edit <rev>` |
| Create and switch | `git checkout -b feature` | `libra checkout -b feature` | `jj new` + `jj branch create` |
| Create from commit | `git checkout -b fix abc1234` | `libra checkout -b fix abc1234` | `jj new abc1234` + `jj branch create fix` |
| Force-create / reset branch | `git checkout -B feature main` | `libra checkout -B feature main` | N/A |
| Auto-track remote | `git checkout feature` (creates tracking) | `libra checkout feature` (creates tracking + pulls) | N/A |
| Restore files | `git checkout -- file` | `libra checkout -- file` (prefer `libra restore file`) | `jj restore` |
| Restore files from revision | `git checkout HEAD -- file` | `libra checkout HEAD -- file` (prefer `libra restore --source HEAD -S -W file`) | `jj restore --from <revision>` |
| Detach HEAD | `git checkout <commit>` / `git checkout --detach <branch>` | `libra checkout <commit>` / `libra checkout -d`/`--detach <branch>` | `jj edit <rev>` |
| Track remote branch | `git checkout -t`/`--track <remote>/<branch>` | `libra checkout -t`/`--track` (accepted no-op; DWIM always tracks) | N/A |
| Structured output | No | `--json` / `--machine` for branch compatibility actions | `--template` |

## Error Handling

`checkout` has a typed `CheckoutError` for checkout-owned failures and delegates path restore failures to `restore` while preserving stable codes.

| Scenario | Stable code | Message | Exit |
|----------|-------------|---------|------|
| Dirty worktree (unstaged or staged changes) | `LBR-REPO-003` | "local changes would be overwritten by checkout" | 128 |
| Untracked file would be overwritten | `LBR-CONFLICT-002` | "local changes would be overwritten by checkout" | 128 |
| Internal branch blocked | `LBR-CLI-003` | "checking out '{name}' branch is not allowed" | 128 |
| Create internal branch blocked | `LBR-CLI-003` | "creating/switching to '{name}' branch is not allowed" | 128 |
| Branch or start-point not found (no remote match) | `LBR-CLI-003` | "path specification '{name}' did not match any files known to libra" | 129 |
| Pathspec not matched in path mode | `LBR-CLI-003` | "pathspec '{path}' did not match any files" | 128 |
| `-b` combined with path mode | `LBR-CLI-002` | "checkout path mode cannot be combined with -b" | 128 |
| Current branch (no-op) | N/A | Prints "Already on {branch}" and succeeds | 0 |
| Branch storage query failure | `LBR-IO-001` | "failed to resolve checkout target: {detail}" | 128 |
| Corrupt branch reference | `LBR-REPO-002` | "failed to resolve checkout target: {detail}" | 128 |
