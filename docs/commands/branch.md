# `libra branch`

Create, delete, rename, inspect, or list branches.

**Alias:** `br`

## Synopsis

```
libra branch [<new_branch>] [<commit_hash>]
libra branch -l [-r | -a] [--contains <commit>] [--no-contains <commit>] [--points-at <object>] [--merged [<commit>]] [--no-merged [<commit>]] [--sort <key>] [--ignore-case] [--column[=<mode>]] [-v | --verbose]
libra branch -d <name>
libra branch -D <name>
libra branch -m [<old>] <new>
libra branch (-c | -C) [<old>] <new>
libra branch -u <upstream>
libra branch --unset-upstream [<branch>]
libra branch --edit-description [<branch>]
libra branch --show-current
```

## Description

`libra branch` manages local and remote-tracking branch references stored in the SQLite database. Without arguments it lists local branches, highlighting the current branch with an asterisk. When given a positional `<new_branch>` argument it creates a new branch pointing at HEAD (or at `<commit_hash>` when provided).

Deletion comes in two flavours: `-d` performs a safe delete that checks whether the branch has been fully merged into the current branch before removing it, while `-D` force-deletes regardless of merge status. Both refuse to delete the branch you are currently on.

The `--contains` and `--no-contains` filters (aliased as `--with` and `--without`) let you narrow the branch list to those whose history does or does not include a particular commit, defaulting to HEAD when the commit argument is omitted. `--points-at <object>` lists branches whose tip is exactly the resolved commit; annotated tag names and full `refs/tags/...` names are peeled recursively to their target commit. `--merged [<commit>]` / `--no-merged [<commit>]` list branches already merged (or not yet merged) into the commit — i.e. whose tip is (or is not) reachable from it, defaulting to HEAD; this is the inverse of `--contains`. `--sort <key>` orders the list by `refname`, `version:refname` (numeric-aware), or `committerdate` / `creatordate` (the tip commit's committer date), with a leading `-` reversing. When the flag is absent, the Git-compatible `branch.sort` config default applies (strict local → global → system cascade; an invalid value fails closed with `LBR-CLI-002` and an unreadable local/global scope with `LBR-IO-001`, before any listing output). Unlike the flag, the config default neither implies `--list` nor suppresses the unborn-HEAD line, matching Git. Documented narrowing: Git stacks repeated `branch.sort` values into a multi-key sort; Libra applies only the last value of the winning scope. `--ignore-case` makes list sorting case-insensitive.

## Options

| Flag | Long | Value | Description |
|------|------|-------|-------------|
| | `<new_branch>` | positional | Create a new branch pointing at HEAD or `<commit_hash>` |
| | `<commit_hash>` | positional (requires `new_branch`) | Base commit for the new branch |
| `-l` | `--list` | | List branches (default when no action is specified) |
| `-D` | `--delete-force` | `<name>` | Force-delete a branch, even if not fully merged |
| `-d` | `--delete` | `<name>` | Safe-delete a branch (must be fully merged) |
| `-u` | `--set-upstream-to` | `<upstream>` | Set upstream tracking for the current branch |
| | `--unset-upstream` | `[branch]` | Remove upstream tracking for the current branch or the named branch |
| | `--edit-description` | `[branch]` | Edit the branch's description (`branch.<name>.description`) in the configured editor; an empty/comment-only buffer unsets it. Defaults to the current branch. |
| | `--show-current` | | Print the current branch name or detached HEAD state |
| `-m` | `--move` | `<old> <new>` or `<new>` | Rename a branch; with one argument renames the current branch |
| `-c` | `--copy` | `<old> <new>` or `<new>` | Copy a branch (and its upstream config) to a new name, keeping the source; fails if the destination exists |
| `-C` | `--copy-force` | `<old> <new>` or `<new>` | Like `-c`, but overwrite the destination if it exists |
| `-r` | `--remotes` | | Show remote-tracking branches only |
| `-a` | `--all` | | Show local and remote-tracking branches |
| | `--contains` | `[commit]` (default HEAD) | Only list branches containing the commit. Alias: `--with` |
| | `--no-contains` | `[commit]` (default HEAD) | Only list branches not containing the commit. Alias: `--without` |
| | `--points-at` | `<object>` | Only list branches whose tip points at the resolved commit; annotated tags are peeled |
| | `--merged` | `[commit]` (default HEAD) | Only list branches already merged into the commit (tip reachable from it) |
| | `--no-merged` | `[commit]` (default HEAD) | Only list branches not yet merged into the commit |
| | `--sort` | `<key>` | Sort the list by `refname`, `version:refname` (`v:refname`), `committerdate`/`creatordate`/`authordate` (the tip commit's committer date — or author date for `authordate`), `objectsize` (the tip object's byte size), or `objectname` (the tip commit's object id); a leading `-` reverses (use `--sort=-committerdate` for the dash form). Overrides the `branch.sort` config default |
| | `--ignore-case` | | Sort branch names case-insensitively where applicable |
| | `--format` | `<format>` | Render each branch with a for-each-ref format string (e.g. `%(refname:short)`, `%(objectname)`, `%(HEAD)`, `%(upstream)`, `%(if)`…`%(end)`). Replaces the default `* name` listing (and `-v`/`--column`); shares the for-each-ref atom engine |
| | `--column[=<mode>]` | `always` / `auto` / `never` | Lay the branch list out in columns instead of one per line (bare `--column` means `always`; `auto` only when stdout is a terminal). Column mode shows plain, uncolored names. |
| | `--no-column` | | Do not lay the branch list out in columns (equivalent to `--column=never`), countermanding an earlier `--column` (last one wins). Branches list one-per-line by default, so on its own this is a no-op. |
| `-v` | `--verbose` | | List each branch with its tip's short sha and commit subject. Repeat (`-vv`) to also show the upstream-tracking segment `[<upstream>: ahead N, behind M]` (counts omitted when the remote-tracking ref has not been fetched; nothing shown for a branch with no configured upstream). Takes precedence over `--column`. |

### Flag examples

```bash
# Create a branch from HEAD
libra branch feature-x

# Create a branch from another branch or commit
libra branch feature-x main
libra branch hotfix abc1234

# List local branches
libra branch -l

# List all branches (local + remote)
libra branch -l -a

# List branches with their tip sha and commit subject
libra branch -v

# List branches containing the latest release tag
libra branch --contains v2.0

# List branches already merged into main (or not yet merged)
libra branch --merged main
libra branch --no-merged main

# Sort branches by name (version-aware), or reversed
libra branch --sort version:refname
libra branch --sort=-refname

# Sort branches by tip commit date (most recent first)
libra branch --sort=-committerdate

# Render each branch with a for-each-ref format string
libra branch --format='%(refname:short) %(objectname:short)'

# List branches that do NOT contain HEAD
libra branch --no-contains

# Safe-delete a merged branch
libra branch -d topic

# Force-delete regardless of merge status
libra branch -D experiment

# Rename current branch
libra branch -m new-name

# Rename any branch
libra branch -m old-name new-name

# Copy a branch (keeping the original)
libra branch -c old-name new-name

# Set upstream tracking
libra branch -u origin/main

# Clear upstream tracking for the current branch
libra branch --unset-upstream

# List branches whose tip is exactly HEAD
libra branch --points-at HEAD

# Show current branch name
libra branch --show-current

# JSON output for agents
libra branch --json --show-current
```

## `branch diff` (Libra extension)

`libra branch diff [<BASE>] [<BRANCH>]` shows what `<BRANCH>` changes
relative to `<BASE>` — tip-to-tip (the working tree is never involved),
byte-identical to `libra diff <BASE>..<BRANCH>`. Defaults: `<BRANCH>` = the
current branch; `<BASE>` = its configured upstream (no upstream → an error
with setup hints). `--merge-base` switches to three-dot semantics. Curated
passthrough: `--stat`, `--name-only`, `--name-status`, `--exit-code` and
`-- <path>...`; the full diff surface lives on `libra diff`. `--json` emits
the diff schema. Exit 0 even with differences (use `--exit-code` for 1),
129 usage/unknown side, 128 fatal. Note: `diff` is a reserved verb —
`libra branch -v diff` is refused rather than creating a branch named
`diff` (use `libra switch -c diff` if you really want one).

## `branch reset` (Libra extension)

`libra branch reset <BRANCH> <TARGET>` moves a **local** branch tip to any
commit-ish through the authoritative SQLite transaction (reference update +
a reflog entry for the branch) — the index and working tree are **never
touched**. The currently checked-out branch is refused (use `libra reset`,
which moves HEAD/index/worktree consistently). Protected or archived
branches (`libra metadata set --branch <b> protect|archive true`) refuse
with `LBR-POLICY-001`; there is no `--force` — lift the flag explicitly
(`metadata unset`), reset, then re-protect (auditable). The same policy is
enforced inside `libra update-ref`'s transaction for `refs/heads/*` updates
and deletes, so plumbing is not a bypass. Identical re-runs within the
operation log's 5-second dedup window are refused. `reset` joins `diff` as
a reserved verb (`libra switch -c reset` still creates such a branch).

## Common Commands

```bash
libra branch feature-x                  # Create a branch from HEAD
libra branch feature-x main             # Create a branch from another branch
libra branch -d topic                   # Delete a fully merged branch
libra branch -D topic                   # Force-delete a branch
libra branch --set-upstream-to origin/main
                                        # Set upstream for the current branch
libra branch --json --show-current      # Structured JSON output for agents
```

## Human Output

- List: prints the branch list with `*` marking the current branch
- Safe delete: `Deleted branch feature (was abc123...)`
- Rename: `Renamed branch 'old' to 'new'`
- Copy: `Copied branch 'old' to 'new'`
- Unset upstream: `Branch 'main' no longer tracks an upstream branch`
- `--show-current`: prints the current branch name, or `HEAD detached at <hash>` when detached

## Structured Output (JSON examples)

`--json` / `--machine` uses `action` to distinguish operations:

```json
{
  "ok": true,
  "command": "branch",
  "data": {
    "action": "create",
    "name": "feature",
    "commit": "abc123..."
  }
}
```

List action:

```json
{
  "ok": true,
  "command": "branch",
  "data": {
    "action": "list",
    "branches": [
      { "name": "main", "current": true, "commit": "abc1234..." },
      { "name": "feature", "current": false, "commit": "def5678..." }
    ]
  }
}
```

Show-current action:

```json
{
  "ok": true,
  "command": "branch",
  "data": {
    "action": "show-current",
    "name": "main",
    "detached": false,
    "commit": "abc1234..."
  }
}
```

Supported actions:

- `list`: `branches`
- `create`: `name`, `commit`
- `delete`: `name`, `commit`, `force`
- `rename`: `old_name`, `new_name`
- `set-upstream`: `branch`, `upstream`
- `unset-upstream`: `branch`
- `show-current`: `name`, `detached`, `commit`

## Design Rationale

### Why no --track/--no-track?

Git's `--track` and `--no-track` flags control whether a new branch automatically sets up an upstream relationship. Libra omits these from `branch` because tracking configuration is handled explicitly through `--set-upstream-to` or at switch time via `libra switch --track`. This separation keeps `branch` focused on ref creation and avoids the confusing implicit behavior where `git branch feature origin/feature` silently configures tracking. When an agent creates a branch, it should know whether tracking was configured -- explicit is better than implicit.

### Why --contains/--no-contains with aliases --with/--without?

The `--contains` and `--no-contains` flags mirror Git for compatibility, but Libra adds shorter `--with` and `--without` aliases. These read more naturally in scripts (`libra branch --with v2.0`) and reduce typing. The flags accept an optional commit argument that defaults to HEAD, which covers the most common case of "which branches include my current work?"

### Why SQLite-backed refs?

Git stores branch references as individual files under `.git/refs/heads/`. This causes problems at scale: monorepos with thousands of branches suffer from filesystem overhead, packed-refs contention, and race conditions during concurrent updates. Libra stores all references in a SQLite database (`libra.db`), which provides:

- **Atomic transactions**: branch create/delete/rename are single-transaction operations with no risk of partial writes or corrupted ref files.
- **Efficient queries**: listing branches, filtering with `--contains`, and upstream lookups are SQL queries rather than directory scans.
- **Concurrency safety**: SQLite's WAL mode handles concurrent reads and serialized writes without external locking.
- **Consistent snapshots**: operations that read multiple refs (like `--contains` filtering) see a consistent view of the ref store.

The trade-off is that refs are not directly inspectable as plain files. Libra compensates with structured JSON output for tooling integration.

## Parameter Comparison: Libra vs Git vs jj

| Feature | Git | Libra | jj |
|---------|-----|-------|----|
| Create branch | `git branch <name>` | `libra branch <name>` | `jj branch create <name>` |
| Create from commit | `git branch <name> <commit>` | `libra branch <name> <commit>` | `jj branch create <name> -r <rev>` |
| List branches | `git branch [-l]` | `libra branch [-l]` | `jj branch list` |
| Delete (safe) | `git branch -d <name>` | `libra branch -d <name>` | `jj branch delete <name>` |
| Delete (force) | `git branch -D <name>` | `libra branch -D <name>` | `jj branch delete <name>` (always force) |
| Rename | `git branch -m <old> <new>` | `libra branch -m <old> <new>` | Not supported |
| Copy | `git branch -c <old> <new>` | `libra branch -c <old> <new>` (`-C` to force) | Not supported |
| Set upstream | `git branch -u <upstream>` | `libra branch -u <upstream>` | N/A (no upstream concept) |
| Unset upstream | `git branch --unset-upstream [branch]` | `libra branch --unset-upstream [branch]` | N/A |
| Show current | `git branch --show-current` | `libra branch --show-current` | `jj log -r @` |
| Remote branches | `git branch -r` | `libra branch -r` | `jj branch list --all` |
| All branches | `git branch -a` | `libra branch -a` | `jj branch list --all` |
| Contains filter | `git branch --contains <commit>` | `libra branch --contains <commit>` | `jj log -r 'branches() & ancestors(<rev>)'` |
| Merged filter | `git branch --merged [<commit>]` / `--no-merged` | `libra branch --merged [<commit>]` / `--no-merged` | `jj log -r 'branches() & ::<rev>'` |
| Points-at filter | `git branch --points-at <object>` | `libra branch --points-at <object>` | N/A |
| Sort list | `git branch --sort <key>` | `libra branch --sort <key>` (refname / version:refname / committerdate / creatordate / authordate / objectsize / objectname) | `jj branch list` (revset order) |
| Custom format | `git branch --format <format>` | `libra branch --format <format>` (for-each-ref atoms; replaces `* name`/`-v`/`--column`) | N/A |
| Column layout | `git branch --column[=<mode>]` | `libra branch --column[=<mode>]` (`--no-column` countermands) | N/A |
| Verbose listing | `git branch -v` / `-vv` | `libra branch -v` (sha + subject) / `-vv` (+ upstream tracking) | N/A |
| Auto-track | `git branch --track` | N/A (use `switch --track`) | N/A |
| Structured output | No | `--json` / `--machine` | `--template` |
| Fuzzy suggestions | No | Levenshtein-based "did you mean" | No |

## Error Handling

| Scenario | Error Code | Hint |
|----------|-----------|------|
| Invalid start point or missing branch | `LBR-CLI-003` | "use 'libra branch -l' to list branches" + fuzzy suggestions |
| Invalid branch name | `LBR-CLI-002` | "branch names cannot contain spaces, '..', '@{', or control characters." |
| Branch already exists | `LBR-CONFLICT-002` | "delete it first or choose a different name." |
| Current branch cannot be deleted | `LBR-REPO-003` | "switch to a different branch first." |
| Branch not fully merged (safe delete) | `LBR-REPO-003` | "use '-D' to force-delete." |
| Locked/internal branch | `LBR-CLI-003` | -- |
| HEAD is detached (rename/upstream) | `LBR-REPO-003` | -- |
| Failed to write refs | `LBR-IO-002` | -- |
| Storage query failed | `LBR-IO-001` | -- |
| Stored reference corrupt | `LBR-REPO-002` | -- |
