# `libra remote`

Manage configured remotes: list, add, remove, rename, inspect and mutate URLs, and prune stale remote-tracking refs.

## Synopsis

```
libra remote <subcommand> [OPTIONS] [ARGS]
libra remote show
libra remote -v
libra remote add [-f | --fetch] [-t | --track <branch>]... [-m | --master <branch>] [--tags | --no-tags] [--mirror] <name> <url>
libra remote remove <name>
libra remote rename <old> <new>
libra remote get-url [--push] [--all] <name>
libra remote set-url [--add | --delete] [--push] [--all] <name> <value>
libra remote prune [--dry-run] <name>
libra remote update [-p | --prune] [<group> | <remote>...]
```

## Description

`libra remote` manages the set of named remotes stored in the SQLite configuration
database. Each remote has one or more fetch URLs and optionally separate push URLs.
Subcommands allow full CRUD operations on remotes and their URLs, as well as pruning
stale remote-tracking branches that no longer exist on the remote.

Remote configuration is stored as `remote.<name>.url` and `remote.<name>.pushurl` keys
in the SQLite `config` table, rather than in a flat `.git/config` file. This provides
transactional safety (no partial writes on crash) and makes remote metadata queryable
by agents and tooling.

## Options

### Subcommand: `show`

With no name, lists configured remote names, one per line. With a `<name>`,
prints detailed information about that remote: configured fetch/push URLs, the
remote HEAD branch, the remote branches, and the local branches/refs configured
for pull/push. By default it **contacts the remote** (like `git remote show`):
the HEAD is the live default branch, branches are classified `tracked` /
`new` / `stale` against the local remote-tracking refs, and `queried` is `true`.

| Flag / Argument | Description | Example |
|-----------------|-------------|---------|
| (none) | Prints all remote names | `libra remote show` |
| `<name>` | Remote to inspect in detail | `libra remote show origin` |
| `-n`, `--no-query` | Do not contact the remote; report the cached HEAD (`refs/remotes/<name>/HEAD`) and cached tracking branches offline (status `cached`, `queried = false`) | `libra remote show --no-query origin` |
| `-v`, `--verbose` | Include additional detail where available | `libra remote show -v origin` |

> Branch classification (online): `tracked` (advertised by the remote and
> already fetched), `new` (advertised but not yet fetched — a later `fetch`
> stores it), `stale` (fetched locally but no longer advertised — `libra remote
> prune` removes it). When the remote is unreachable, `show` fails with a hint to
> retry with `--no-query`. For multi-remote fetching, see the `update`
> subcommand.

### Subcommand: `-v` (list verbose)

List every remote with its fetch and push URLs.

| Argument | Description |
|----------|-------------|
| (none) | Prints `<name>\t<url> (fetch\|push)` for each URL |

### Subcommand: `add`

Register a new remote.

| Argument | Description | Example |
|----------|-------------|---------|
| `<name>` | Logical name for the remote | `origin` |
| `<url>` | Fetch URL for the remote | `https://example.com/repo.git` |
| `-f`, `--fetch` | Fetch from the new remote immediately after adding it | |
| `-t`, `--track <branch>` | Track only the given branch — writes a specific `remote.<name>.fetch` refspec instead of the default wildcard. Repeatable. | `-t main -t dev` |
| `-m`, `--master <branch>` | Point the remote's HEAD (`refs/remotes/<name>/HEAD`) at `<branch>` (written even before the tracking ref exists, like Git) | `-m main` |
| `--tags` / `--no-tags` | Set `remote.<name>.tagOpt` to fetch all / no tags (mutually exclusive) | |
| `--mirror` | Mark the remote as a mirror — writes the `remote.<name>.mirror=true` marker (like Git's `remote add --mirror=fetch`). Incompatible with `-t`/`--track`. | `--mirror` |

The `--mirror` marker is informational: Libra does **not** write a `+refs/*:refs/*` fetch refspec because `libra fetch` is not yet mirror-aware (matching `libra clone --mirror`).

### Subcommand: `remove`

Delete a remote and all its configuration keys.

| Argument | Description | Example |
|----------|-------------|---------|
| `<name>` | Name of the remote to remove | `origin` |

### Subcommand: `rename`

Rename an existing remote.

| Argument | Description | Example |
|----------|-------------|---------|
| `<old>` | Current name | `origin` |
| `<new>` | New name | `upstream` |

The rename is one SQLite transaction. It rewrites `remote.<old>.*` keys,
branch upstream values, SSH credential namespaces, configured fetch-refspec
destinations, cached `refs/remotes/<old>/*` branches and remote HEAD rows, and
their reflog names. Any failed rewrite rolls the complete rename back.

### Subcommand: `get-url`

Print URLs configured for a remote.

| Flag / Argument | Description | Example |
|-----------------|-------------|---------|
| `<name>` | Remote name | `origin` |
| `--push` | Print push URLs instead of fetch URLs | `libra remote get-url --push origin` |
| `--all` | Print all configured URLs (not just the first) | `libra remote get-url --all origin` |

### Subcommand: `set-url`

Add, replace, or delete URLs for a remote.

| Flag / Argument | Description | Example |
|-----------------|-------------|---------|
| `<name>` | Remote name | `origin` |
| `<value>` | URL value (or substring pattern for `--delete`) | `https://mirror.example.com/repo.git` |
| `--add` | Append a new URL rather than replacing | `libra remote set-url --add origin https://mirror.example.com/repo.git` |
| `--delete` | Remove URLs matching the given substring | `libra remote set-url --delete origin mirror` |
| `--push` | Operate on push URLs (`pushurl`) instead of fetch URLs (`url`) | `libra remote set-url --push origin ssh://git@example.com/repo.git` |
| `--all` | Apply replacement to all matching entries | `libra remote set-url --all origin https://new.example.com/repo.git` |

### Subcommand: `prune`

Delete local remote-tracking branches that no longer exist on the remote.

| Flag / Argument | Description | Example |
|-----------------|-------------|---------|
| `<name>` | Remote name | `origin` |
| `--dry-run` | Show what would be pruned without deleting | `libra remote prune --dry-run origin` |

### Subcommand: `update`

Fetch from one or more remotes. With no arguments, a non-empty
`remotes.default` value is resolved first (its whitespace-separated entries may
be remote names or `remotes.<group>` names); if it is unset or empty, every
configured remote is fetched. Explicit arguments use the same remote/group
resolution.

| Flag / Argument | Description | Example |
|-----------------|-------------|---------|
| `-p`, `--prune` | After fetching, prune remote-tracking branches that no longer exist on the remote (Git's `remote update -p`) | `libra remote update -p` |
| `[<group> \| <remote>...]` | Remotes or remote groups to fetch (default: `remotes.default`, otherwise all) | `libra remote update origin upstream` |

> `-p` / `--prune` runs the same prune logic as `libra remote prune <name>`, but
> only after every resolved remote has fetched successfully (a two-pass
> fetch-all-then-prune, so a later fetch failure never strands an earlier
> prune), reporting any removed refs as `* [pruned] <name>/<branch>`.

### Subcommand: `set-branches`

Set the branches tracked by a remote by rewriting its `remote.<name>.fetch`
refspecs. Each branch becomes `+refs/heads/<branch>:refs/remotes/<name>/<branch>`.
Subsequent `fetch <name>` and `remote update` consume these values and update
only the configured branches.

| Flag / Argument | Description | Example |
|-----------------|-------------|---------|
| `<name>` | Remote name | `origin` |
| `<branch>...` | One or more branch names to track (required) | `libra remote set-branches origin main dev` |
| `--add` | Append to the tracked branches instead of replacing them | `libra remote set-branches --add origin dev` |

### Subcommand: `set-head`

Set or delete a remote's default branch pointer (`refs/remotes/<name>/HEAD`).

| Flag / Argument | Description | Example |
|-----------------|-------------|---------|
| `<name>` | Remote name | `origin` |
| `<branch>` | Branch to set as the remote HEAD (must already exist as a tracking branch) | `libra remote set-head origin main` |
| `-d`, `--delete` | Delete the remote HEAD ref (idempotent) | `libra remote set-head origin -d` |
| `-a`, `--auto` | Query the remote and set its HEAD to the branch the remote points at | `libra remote set-head origin -a` |

> The three modes (`<branch>`, `--delete`, `--auto`) are mutually exclusive.
> Both `set-head <branch>` and `--auto` require the resolved branch's tracking
> ref `refs/remotes/<name>/<branch>` to already exist (fetch it first); `--auto`
> additionally contacts the remote to discover its default branch. `remote
> update [-p|--prune] [<group>|<remote>...]` fetches all configured remotes (or
> the named ones, expanding any `remotes.<group>` config); `-p`/`--prune` then
> prunes stale remote-tracking branches after every fetch succeeds.

## Common Commands

```bash
libra remote show
libra remote show origin
libra remote -v
libra remote add origin https://example.com/repo.git
libra remote get-url origin
libra remote get-url --all origin
libra remote set-url --add origin https://mirror.example.com/repo.git
libra remote set-url --add --push origin ssh://git@example.com/repo.git
libra remote prune --dry-run origin
libra remote set-branches origin main dev
libra remote set-head origin main
libra remote set-head origin --auto
libra remote set-head origin -d
libra remote show origin
libra remote show --no-query origin
```

## Human Output

- `remote show` (no name) prints configured remote names, one per line.
- `remote show <name>` queries the remote and prints a detailed report
  (branches classified `tracked` / `new` / `stale`):

```text
* remote origin
  Fetch URL: https://example.com/repo.git
  Push URL: https://example.com/repo.git
  HEAD branch: main
  Remote branches:
    main tracked
    feature-x new (next fetch will store in remotes/origin)
    old-topic stale (use 'libra remote prune' to remove)
  Local branches configured for 'git pull':
    (none)
  Local refs configured for 'git push':
    (none)
```

- `remote show --no-query <name>` stays offline; it prints `Remote branch data:
  cached` and reports cached tracking branches with the `cached` status.

- `remote -v` prints every fetch URL and effective push URL:

```text
origin  https://example.com/repo.git (fetch)
origin  ssh://git@example.com/repo.git (push)
```

- `remote add` prints `Added remote 'origin' -> https://example.com/repo.git`
- `remote remove` prints `Removed remote 'origin'`
- `remote rename` prints `Renamed remote 'origin' to 'upstream'`
- `remote get-url` prints the selected URL set, one per line
- `remote set-url` prints a confirmation describing whether a URL was added, replaced, or deleted
- `remote prune` prints each pruned branch and a final summary; `--dry-run` uses `[would prune]`

```text
 * [would prune] origin/stale-feature
 * [would prune] origin/old-experiment

Would prune 2 stale remote-tracking branch(es).
```

## Structured Output (JSON examples)

- `--json` writes one success envelope to `stdout`
- `--machine` writes the same schema as compact single-line JSON
- action-specific payloads are tagged with `data.action`

### Action Schemas

- `add`: `name`, `url`
- `remove`: `name`
- `rename`: `old_name`, `new_name`
- `list`: `verbose`, `remotes[]`
- `urls`: `name`, `push`, `all`, `urls[]`
- `set-url`: `name`, `role`, `mode`, `urls[]`, `removed`
- `prune`: `name`, `dry_run`, `stale_branches[]`
- `update`: `remotes[]` (names fetched), `pruned[]` (each `{remote_ref, branch}`; present only with `-p`/`--prune` and omitted entirely when nothing was pruned)
- `show`: `name`, `fetch_urls[]`, `push_urls[]`, `head_branch`, `remote_branches[]` (each `{branch, status, local_oid, remote_oid}`; `status` is `tracked`/`new`/`stale` online or `cached` with `--no-query`), `pull_config[]`, `push_config[]`, `queried` (`true` when the remote was contacted, `false` with `--no-query`)
- `set-branches`: `name`, `added`, `fetch_refspecs[]`
- `set-head`: `name`, `mode` (`set`/`delete`), `target`

Example (verbose list):

```json
{
  "ok": true,
  "command": "remote",
  "data": {
    "action": "list",
    "verbose": true,
    "remotes": [
      {
        "name": "origin",
        "fetch_urls": ["https://example.com/repo.git"],
        "push_urls": ["ssh://git@example.com/repo.git"]
      }
    ]
  }
}
```

Example (prune dry-run):

```json
{
  "ok": true,
  "command": "remote",
  "data": {
    "action": "prune",
    "name": "origin",
    "dry_run": true,
    "stale_branches": [
      {
        "remote_ref": "refs/remotes/origin/stale-feature",
        "branch": "origin/stale-feature"
      }
    ]
  }
}
```

### Schema Notes

- `list.remotes[].fetch_urls` contains all configured fetch URLs
- `list.remotes[].push_urls` contains effective push URLs; when no explicit `pushurl` is configured it falls back to fetch URLs
- `prune.stale_branches[].branch` is the user-facing short name such as `origin/feature`
- `remote show` currently maps to `action = "list"` with `verbose = false`

## Design Rationale

### Why SQLite-backed remote storage?

Git stores remote configuration in the flat-file `.git/config` using INI-style syntax.
This format is easy to hand-edit but has no transactional guarantees: a crash mid-write
can leave the file truncated or corrupt. Libra stores remotes in SQLite (`config` table),
which provides ACID transactions, concurrent-read safety, and structured queries. An
agent can enumerate all remotes with a single SQL query instead of parsing INI syntax.
The trade-off is that remotes are not directly editable with a text editor, but
`libra remote` subcommands and `libra config` provide full programmatic access.

### Why a `show` subcommand?

Git overloads `git remote` (no subcommand) to list remote names and `git remote -v` for
verbose output. Libra makes listing explicit via `remote show` (names only) and
`remote -v` (verbose with URLs). The `show` subcommand provides a clear, discoverable
entry point for agents that need to enumerate remotes without parsing verbose URL output.
It also avoids the ambiguity of a bare command that means different things depending on
flags.

### Why multi-URL support?

A single remote can have multiple fetch URLs and separate push URLs. This enables
mirror-push workflows (push to GitHub and a self-hosted GitLab simultaneously) and
read-from-cache patterns (fetch from a local mirror, push to the canonical remote).
The `set-url --add` and `set-url --delete` flags manage URL lists without requiring
manual config editing. The `get-url --all` flag exposes the full URL set for inspection.
Push URLs (`pushurl`) take precedence when configured; otherwise, fetch URLs are used
for both fetch and push, matching Git's behavior.

## Parameter Comparison: Libra vs Git vs jj

| Operation | Libra | Git | jj |
|-----------|-------|-----|----|
| List names | `libra remote show` | `git remote` | `jj git remote list` |
| List with URLs | `libra remote -v` | `git remote -v` | `jj git remote list` (always verbose) |
| Add remote | `libra remote add <n> <u>` | `git remote add <n> <u>` | `jj git remote add <n> <u>` |
| Add remote + fetch | `libra remote add -f <n> <u>` | `git remote add -f <n> <u>` | N/A |
| Add mirror remote | `libra remote add --mirror <n> <u>` (marker only) | `git remote add --mirror=fetch <n> <u>` | N/A |
| Remove remote | `libra remote remove <n>` | `git remote remove <n>` | `jj git remote remove <n>` |
| Rename remote | `libra remote rename <o> <n>` | `git remote rename <o> <n>` | `jj git remote rename <o> <n>` |
| Get URL | `libra remote get-url <n>` | `git remote get-url <n>` | N/A |
| Set URL | `libra remote set-url <n> <u>` | `git remote set-url <n> <u>` | N/A |
| Add extra URL | `libra remote set-url --add <n> <u>` | `git remote set-url --add <n> <u>` | N/A |
| Delete URL | `libra remote set-url --delete <n> <p>` | `git remote set-url --delete <n> <p>` | N/A |
| Push-specific URL | `--push` flag on get-url/set-url | `--push` flag on get-url/set-url | N/A |
| Prune stale refs | `libra remote prune <n>` | `git remote prune <n>` | Automatic |
| Prune dry-run | `libra remote prune --dry-run <n>` | `git remote prune --dry-run <n>` | N/A |
| Storage backend | SQLite (transactional) | Flat file (.git/config) | TOML + oplog |
| Structured output | `--json` / `--machine` | No | No |

## Error Handling

| Scenario | StableErrorCode | Exit | Hint |
|----------|-----------------|------|------|
| Duplicate remote name | `LBR-CONFLICT-002` | 128 | "use 'libra remote -v' to inspect configured remotes" |
| Remote not found | `LBR-CLI-003` | 129 | "use 'libra remote -v' to inspect configured remotes" |
| No URL configured for remote | `LBR-CLI-003` | 129 | "use 'libra remote get-url --all \<name>' to inspect configured URLs" |
| URL pattern not matched (`set-url --delete`) | `LBR-CLI-003` | 129 | "use 'libra remote get-url --all \<name>' to inspect configured URLs" |
| Failed to read remote config | `LBR-IO-001` | 128 | -- |
| Failed to update remote config | `LBR-IO-002` | 128 | -- |
| Failed to list remote-tracking branches | `LBR-IO-001` | 128 | -- |
| Corrupt remote-tracking branch | `LBR-REPO-002` | 128 | -- |
| Failed to prune remote-tracking branch | `LBR-IO-002` | 128 | -- |
| Remote object format mismatch during prune | `LBR-REPO-003` | 128 | "remote uses a different hash algorithm" |
| Remote discovery / auth / network failure during prune | fetch-aligned network/auth codes | 128 | See `libra fetch` error table |
