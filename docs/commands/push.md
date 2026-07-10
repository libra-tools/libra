# `libra push`

Send local commits and objects to a remote repository, updating remote refs.
Supports SSH and HTTPS transports, LFS file uploads (HTTP only), fast-forward detection,
force push, dry-run preview, multi-refspec updates, remote ref deletion, tag pushing,
and mirror previews.

## Synopsis

```
libra push [OPTIONS] [<repository> [<refspec>...]]
```

## Description

`libra push` transfers commits, trees, blobs, and tags from the local repository to a
remote. When invoked without arguments it pushes the current branch to its configured
upstream remote. When a `repository` and one or more `refspec` values are given, all
refspecs are validated before any network write and then sent in one receive-pack
request. `--tags` pushes all local tags, and `--mirror` mirrors local branch/tag refs
to the remote, including deletion of remote-only refs.

The command negotiates with the remote to determine which objects are missing, packs them
into a single pack file, and sends the pack along with a ref-update request. If the remote
ref has diverged (non-fast-forward), the push is rejected unless `--force` is used.

`--force-with-lease` is the safe alternative to `--force`: it allows a non-fast-forward
update only if the remote ref still matches the OID you expected. By default the expected
OID is your local remote-tracking ref (`refs/remotes/<remote>/<branch>`), so a force that
would clobber a teammate's newer commit is rejected. The check runs after discovery and
**before** any object collection, LFS upload, or pack send — a failed lease changes nothing
on either side.

`--porcelain` prints a stable, machine-readable line per ref instead of the human summary.

LFS-tracked files are transparently uploaded during HTTP pushes without requiring a
separate `lfs push` step.

## Global Config Schema Guard

`libra push` reads the global storage configuration (`~/.libra/config.db`, or
`LIBRA_CONFIG_GLOBAL_DB`) before trusting remote/tiered object storage settings. If that
database has a schema version newer than this binary supports, push fails closed with
`LBR-CONFIG-001` instead of silently ignoring global storage config and falling back to
local objects. The diagnostic includes the binary path and version, config DB path,
schema versions, and the update command:
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`.

Use `libra --offline push ...` or `LIBRA_READ_POLICY=offline|local libra push ...` only when
you intentionally want local-only object access. Libra will warn once and ignore the
global storage config for that run.

## Options

| Flag / Argument | Description | Example |
|-----------------|-------------|---------|
| `<repository>` | Remote name (e.g. `origin`). Required when `<refspec>`, `--tags`, or `--mirror` is used. | `libra push origin main` |
| `<refspec>...` | Local ref, `<src>:<dst>` mapping, or `:<dst>` deletion. Multiple values are sent as one update set. | `libra push origin main feature:release` |
| `-u`, `--set-upstream` | Set the upstream tracking branch after a successful single branch push. | `libra push -u origin feature-x` |
| `-f`, `--force` | Allow non-fast-forward updates that overwrite remote history. | `libra push --force origin main` |
| `-d`, `--delete` | Delete the named remote refs (each `<refspec>` is rewritten to a `:<ref>` deletion). Requires at least one ref; conflicts with `--set-upstream`/`--tags`/`--mirror`. | `libra push -d origin feature-x` |
| `--force-with-lease[=<ref>[:<expect>]]` | Allow a non-fast-forward update only if the remote ref still matches the expected OID (the tracking-ref OID by default, or an explicit `<expect>`). Conflicts with `--force`. | `libra push --force-with-lease origin main` |
| `--force-if-includes` | With `--force-with-lease` (All/Ref forms): additionally require the remote-tracking tip to be integrated locally (reachable from the pushed branch's reflog). Silent no-op with the exact lease form or without a lease (Git parity). |
| `--thin` | Send REF_DELTA entries against server-known bases (the advertised old tips) — smaller packs on large-blob edits; the server completes them (`index-pack --fix-thin`). Self-contained packs remain the default (unlike git). |
| `--no-verify` | Bypass the `pre-push` hook. Accepted for compatibility; **no-op** (Libra's push runs no client-side `pre-push` hook, so there is nothing to bypass). | `libra push --no-verify origin main` |
| `--no-progress` | Suppress the progress meter (the "Compressing objects" / "Writing objects" reporters) on stderr, matching `git push --no-progress`. | `libra push --no-progress origin main` |
| `--porcelain` | Machine-readable output: a `To <url>` header then `<flag>\t<from>:<to>\t<summary>` per ref. Conflicts with `--json`/`--machine`. | `libra push --porcelain origin main` |
| `-n`, `--dry-run` | Perform negotiation and object collection but skip the actual upload. Reports what would be pushed. | `libra push --dry-run` |
| `--tags` | Push all local `refs/tags/*` refs. Existing identical remote tags are skipped. | `libra push --tags origin` |
| `--mirror` | Mirror local `refs/heads/*` and `refs/tags/*` to the remote, deleting remote-only branch/tag refs. Use with `--dry-run` to preview. | `libra push --mirror --dry-run origin` |
| `--json` | Emit structured JSON envelope to stdout (global flag). | `libra push --json` |
| `--machine` | Compact single-line JSON; suppresses progress (global flag). | `libra push --machine` |
| `--quiet` | Suppress stdout summary; warnings still go to stderr. | `libra push --quiet` |

## Common Commands

```bash
libra push
libra push origin main
libra push -u origin feature-x
libra push --force origin main
libra push --force-with-lease origin main
libra push --force-with-lease=main:abc123 origin main
libra push --porcelain origin main
libra push --dry-run
libra push origin local_branch:release
libra push origin main feature:release
libra push origin :stale-branch
libra push origin refs/tags/v1.0:refs/tags/v1.0
libra push --tags origin
libra push --mirror --dry-run origin
libra push --json
```

## Human Output

Default human mode writes progress to `stderr` and the push summary to `stdout`.

Normal push:

```text
To git@github.com:user/repo.git
   abc1234..def5678  main -> main
 256 objects pushed (1.2 MiB)
```

New branch:

```text
To git@github.com:user/repo.git
 * [new branch]      feature-x -> feature-x
 12 objects pushed (48.0 KiB)
```

Delete remote ref:

```text
To git@github.com:user/repo.git
 - [deleted]         stale-branch
```

New tag:

```text
To git@github.com:user/repo.git
 * [new tag]      v1.0 -> v1.0
```

Up-to-date:

```text
Everything up-to-date
```

Force push:

```text
To git@github.com:user/repo.git
 + abc1234...def5678 main -> main (forced update)
 128 objects pushed (512.0 KiB)
warning: force push overwrites remote history
```

Dry-run:

```text
To git@github.com:user/repo.git
   abc1234..def5678  main -> main (dry run)
 256 objects would be pushed
```

Set upstream:

```text
To git@github.com:user/repo.git
   abc1234..def5678  main -> main
 256 objects pushed (1.2 MiB)
branch 'main' set up to track 'origin/main'
```

`--quiet` suppresses `stdout` but preserves warnings (e.g. force push) on `stderr`.

## Structured Output (JSON examples)

`libra push` supports the global `--json` and `--machine` flags.

- `--json` writes one success envelope to `stdout`
- `--machine` writes the same schema as compact single-line JSON
- progress output is suppressed in JSON/machine mode
- `stderr` stays clean on success

Example:

```json
{
  "ok": true,
  "command": "push",
  "data": {
    "remote": "origin",
    "url": "git@github.com:user/repo.git",
    "updates": [
      {
        "kind": "update",
        "local_ref": "refs/heads/main",
        "remote_ref": "refs/heads/main",
        "old_oid": "abc1234...",
        "new_oid": "def5678...",
        "forced": false
      }
    ],
    "objects_pushed": 256,
    "bytes_pushed": 1258291,
    "lfs_files_uploaded": 0,
    "dry_run": false,
    "up_to_date": false,
    "upstream_set": null,
    "warnings": []
  }
}
```

Up-to-date:

```json
{
  "ok": true,
  "command": "push",
  "data": {
    "remote": "origin",
    "url": "git@github.com:user/repo.git",
    "updates": [],
    "objects_pushed": 0,
    "bytes_pushed": 0,
    "lfs_files_uploaded": 0,
    "dry_run": false,
    "up_to_date": true,
    "upstream_set": null,
    "warnings": []
  }
}
```

Dry-run:

```json
{
  "ok": true,
  "command": "push",
  "data": {
    "remote": "origin",
    "url": "git@github.com:user/repo.git",
    "updates": [
      {
        "kind": "update",
        "local_ref": "refs/heads/main",
        "remote_ref": "refs/heads/main",
        "old_oid": "abc1234...",
        "new_oid": "def5678...",
        "forced": false
      }
    ],
    "objects_pushed": 256,
    "bytes_pushed": 0,
    "lfs_files_uploaded": 0,
    "dry_run": true,
    "up_to_date": false,
    "upstream_set": null,
    "warnings": []
  }
}
```

Force push:

```json
{
  "ok": true,
  "command": "push",
  "data": {
    "remote": "origin",
    "url": "git@github.com:user/repo.git",
    "updates": [
      {
        "kind": "update",
        "local_ref": "refs/heads/main",
        "remote_ref": "refs/heads/main",
        "old_oid": "abc1234...",
        "new_oid": "def5678...",
        "forced": true
      }
    ],
    "objects_pushed": 128,
    "bytes_pushed": 524288,
    "lfs_files_uploaded": 0,
    "dry_run": false,
    "up_to_date": false,
    "upstream_set": null,
    "warnings": ["force push overwrites remote history"]
  }
}
```

Set upstream:

```json
{
  "ok": true,
  "command": "push",
  "data": {
    "remote": "origin",
    "url": "git@github.com:user/repo.git",
    "updates": [
      {
        "kind": "update",
        "local_ref": "refs/heads/main",
        "remote_ref": "refs/heads/main",
        "old_oid": "abc1234...",
        "new_oid": "def5678...",
        "forced": false
      }
    ],
    "objects_pushed": 256,
    "bytes_pushed": 1258291,
    "lfs_files_uploaded": 0,
    "dry_run": false,
    "up_to_date": false,
    "upstream_set": "origin/main",
    "warnings": []
  }
}
```

### Schema Notes

- `updates` lists each ref update; empty when up-to-date
- `kind` is `update` for branch/tag updates and `delete` for remote ref deletion
- delete updates use an empty `local_ref` and the all-zero object id as `new_oid`
- `old_oid` is `null` for new branches (no previous remote ref)
- `forced` is `true` when the update required `--force` (non-fast-forward)
- `bytes_pushed` is the pack data size in bytes; `0` for dry-run
- `lfs_files_uploaded` counts LFS objects transferred (HTTP transport only)
- `upstream_set` is non-null when `-u` / `--set-upstream` was used
- `warnings` contains force push warnings or other advisory messages

## Porcelain Output

`--porcelain` prints a stable, script-parseable format (mutually exclusive with
`--json`/`--machine`). The first line is `To <url>` (credential-redacted), then one
tab-separated line per ref:

```text
<flag>\t<from>:<to>\t<summary>
```

The leading flag follows `git push --porcelain`:

| Flag | Meaning | Example summary |
|------|---------|-----------------|
| ` ` (space) | Fast-forward update | `abc1234..def5678` |
| `+` | Forced (non-fast-forward) update | `abc1234...def5678 (forced update)` |
| `*` | New ref created | `[new branch]` / `[new tag]` |
| `-` | Ref deleted | `[deleted]` |

Rejected refs (`!`) do not appear here: a rejected push fails with a typed error on
stderr (see Error Handling) rather than a partial-success porcelain report.

## Force-with-lease

`--force-with-lease` accepts three forms (matching Git):

- bare `--force-with-lease` — every pushed ref must still match its remote-tracking
  ref (`refs/remotes/<remote>/<branch>`).
- `--force-with-lease=<ref>` — only `<ref>` is checked, against its tracking ref.
- `--force-with-lease=<ref>:<expect>` — `<ref>` is checked against the explicit
  `<expect>` OID (which may be abbreviated).

A lease mismatch is reported as a non-fast-forward rejection (`LBR-CONFLICT-002`, exit
`128`) before any object is collected, packed, or sent. `--force` and `--force-with-lease`
are mutually exclusive (clap rejects the combination, exit `2`).

## Refspec Semantics

The following forms are supported:

| Invocation | Meaning |
|-----------|---------|
| `libra push` | Push current branch to its configured tracking remote |
| `libra push origin main` | Push local `refs/heads/main` to remote `refs/heads/main` |
| `libra push origin local:release` | Push local `refs/heads/local` to remote `refs/heads/release` |
| `libra push origin main feature:release` | Validate and send multiple ref updates together |
| `libra push origin :feature` | Delete remote `refs/heads/feature` |
| `libra push -d origin feature` | Delete remote `refs/heads/feature` (short form) |
| `libra push origin refs/tags/v1.0:refs/tags/v1.0` | Push a tag ref |
| `libra push --tags origin` | Push all local tag refs |
| `libra push --mirror --dry-run origin` | Preview mirroring branch/tag refs and deleting remote-only refs |

Empty destination syntax (`src:`), malformed ref names, duplicate destination refs,
and `--mirror` combined with explicit refspecs are rejected before any network write.
Invalid forms return `InvalidRefspec` with exit 129.

## Design Rationale

### Why require an explicit repository+refspec pair?

Git allows `git push origin` (push current branch to same-named remote branch) and treats
`repository` and `refspec` as independent optional arguments with complex defaulting rules
(`push.default`, `remote.pushDefault`, branch tracking config). This flexibility is a
well-known source of accidental pushes to the wrong branch. Libra takes a deliberately
restrictive stance: when you name a remote you must also name the ref. The bare
`libra push` form (no arguments) uses the tracking configuration, which is unambiguous.
This eliminates an entire class of "I accidentally pushed to production" mistakes without
reducing the expressiveness of the command for scripted or agent-driven workflows.

### Why keep local file remotes rejected?

Libra still treats local file remote push as an intentionally different surface. The
C8 ref update expansion applies to network receive-pack transports; local-path remotes
continue to fail closed to avoid undefined concurrent filesystem mutation semantics.

### Why integrated LFS push?

Git LFS requires a separate binary (`git-lfs`) and a post-push hook to upload large files.
This two-phase design means LFS failures can leave the remote in an inconsistent state
where commits reference LFS pointers whose backing objects have not arrived. Libra detects
LFS pointer blobs during the object-collection phase and uploads them inline during the
HTTP push transaction. This ensures atomicity: either all objects (including LFS) arrive,
or the push fails cleanly. The integration is transparent -- users do not need to install
or configure a separate LFS tool.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Basic push | `libra push` | `git push` | `jj git push` |
| Named remote + ref | `libra push origin main` | `git push origin main` | `jj git push --remote origin --branch main` |
| Set upstream | `libra push -u origin main` | `git push -u origin main` | N/A (jj tracks bookmarks) |
| Force push | `libra push --force` | `git push --force` | `jj git push --allow-new` |
| Lease-protected force | `libra push --force-with-lease` | `git push --force-with-lease` | N/A |
| Force-if-includes | Accepted, no-op | `git push --force-if-includes` | N/A |
| Porcelain output | `libra push --porcelain` | `git push --porcelain` | N/A |
| Thin pack | Accepted, no-op | `git push --thin` | N/A |
| Skip pre-push hook | Accepted, no-op | `git push --no-verify` | N/A |
| Suppress progress | `libra push --no-progress` | `git push --no-progress` | N/A |
| Atomic / signed / push-option / follow-tags | Not yet supported | `git push --atomic` / `--signed` / `-o` / `--follow-tags` | N/A |
| Dry-run | `libra push --dry-run` | `git push --dry-run` | `jj git push --dry-run` |
| Refspec mapping | `libra push origin src:dst` | `git push origin src:dst` | N/A |
| Multiple refspecs | `libra push origin main feature:release` | `git push origin main feature:release` | N/A |
| Delete remote branch | `libra push -d origin branch` or `libra push origin :branch` | `git push -d origin branch` / `git push origin :branch` | `jj git push --delete branch` |
| Push tags | `libra push --tags origin` | `git push --tags origin` | N/A |
| Mirror preview | `libra push --mirror --dry-run origin` | `git push --mirror --dry-run origin` | N/A |
| Structured output | `--json` / `--machine` | No | No |
| Remote name suggestion | Fuzzy match "did you mean?" | No | No |
| Error hints | Every error type has an actionable hint | Minimal | Minimal |
| LFS integration | Transparent during HTTP push | `git lfs push` (separate) | N/A |

## Error Handling

Every `PushError` variant maps to an explicit `StableErrorCode`. Remote name typos
trigger a fuzzy match suggestion via edit distance.

| Scenario | Error Code | Exit | Hint |
|----------|-----------|------|------|
| HEAD is detached | `LBR-REPO-003` | 128 | "checkout a branch before pushing" |
| No remote configured | `LBR-REPO-003` | 128 | "use 'libra remote add' to configure a remote" |
| Remote not found | `LBR-CLI-003` | 129 | "use 'libra remote -v'" + fuzzy "did you mean?" |
| Invalid refspec | `LBR-CLI-002` | 129 | "use '\<name>' or '\<src>:\<dst>'" |
| Source ref not found | `LBR-CLI-003` | 129 | "verify the local branch/ref exists" |
| Local file remote | `LBR-CLI-003` | 129 | "push supports network remotes only" |
| Invalid remote URL | `LBR-CLI-002` | 129 | "check the remote URL" |
| Authentication failed | `LBR-AUTH-001` | 128 | "check SSH key or HTTP credentials" |
| Discovery failed | `LBR-NET-001` | 128 | "check the remote URL and network connectivity" |
| Network timeout | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| Non-fast-forward | `LBR-CONFLICT-002` | 128 | "pull first, or use --force (data loss risk)" |
| Object collection failed | `LBR-INTERNAL-001` | 128 | Issues URL |
| Pack encoding failed | `LBR-INTERNAL-001` | 128 | Issues URL |
| Remote unpack failed | `LBR-NET-002` | 128 | "retry or check server logs" |
| Remote ref update rejected | `LBR-NET-002` | 128 | "check branch protection rules" |
| Network error | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| LFS upload failed | `LBR-NET-001` | 128 | "check LFS endpoint configuration" |
| Tracking ref update failed | `LBR-IO-002` | 128 | -- |
| Repository state error | `LBR-REPO-002` | 128 | "try 'libra status' to verify" |

### Timeout Policy

- Discovery / connection: 60s connection timeout
- Upload / receive-pack: 600s idle timeout (no data progress triggers timeout)
- Timeouts are mapped to `NetworkUnavailable` with `phase` detail
