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
upstream remote. A configured local upstream (`branch.<name>.remote=.`) is refused
before any network write (`LBR-CLI-003`, exit 129; Git `push` is 128 — intentional).
Network support for local upstreams is deferred to [issues/480 HP-16](https://github.com/libra-tools/libra/issues/480). An explicit repository argument `.` keeps the existing `remote '.' not found` path. The `repository` may be an anonymous local-path / `file://` URL spec, which is resolved as a remote and reaches the local-push target check (issues/480 HP-06). When a `repository` and one or more `refspec` values are given, all
refspecs are validated before any network write and then sent in one receive-pack
request. `--tags` pushes all local tags, and `--mirror` mirrors local branch/tag refs
to the remote, including deletion of remote-only refs.

The command negotiates with the remote to determine which objects are missing, packs them
into a single pack file, and sends the pack along with a ref-update request. If the remote
ref has diverged (non-fast-forward), the push is rejected unless `--force` is used.

Object selection reuses every advertised remote ref whose object is available locally, not
only the old value of the ref being updated. Consequently, a new branch or tag that points
at an already-advertised commit sends zero objects instead of repacking that commit's history.
Invalid or locally unavailable advertised OIDs are ignored conservatively. A real zero-object
ref update still sends the protocol-required empty pack: 32 bytes for SHA-1 repositories or
44 bytes for SHA-256 repositories.

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

Configuration schema compatibility is role-scoped. Before `libra push` trusts
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
| `<repository>` | Remote name (e.g. `origin`). Required when `<refspec>`, `--tags`, or `--mirror` is used. | `libra push origin main` |
| `<refspec>...` | Local ref, `<src>:<dst>` mapping, or `:<dst>` deletion. Multiple values are sent as one update set. | `libra push origin main feature:release` |
| `-u`, `--set-upstream` | Set the upstream tracking branch after a successful single branch push. | `libra push -u origin feature-x` |
| `-f`, `--force` | Allow non-fast-forward updates that overwrite remote history. | `libra push --force origin main` |
| `-d`, `--delete` | Delete the named remote refs (each `<refspec>` is rewritten to a `:<ref>` deletion). Short names resolve against the refs the remote advertises — `refs/heads/<name>` first, then `refs/tags/<name>`; an ambiguous name is refused. Requires at least one ref; conflicts with `--set-upstream`/`--tags`/`--mirror`. | `libra push -d origin feature-x` |
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
- `objects_pushed` counts objects in the generated pack; it can be `0` for a new ref whose target the remote already advertised
- `bytes_pushed` is the pack data size in bytes; it is `0` for dry-run, while a real zero-object update reports the 32-byte SHA-1 or 44-byte SHA-256 empty pack
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
| `libra push -d origin v1.0` | Delete remote `refs/tags/v1.0` if no branch named `v1.0` is advertised (short-name deletion tries `refs/heads/<name>` then `refs/tags/<name>`) |
| `libra push origin refs/tags/v1.0:refs/tags/v1.0` | Push a tag ref |
| `libra push --tags origin` | Push all local tag refs |
| `libra push --mirror --dry-run origin` | Preview mirroring branch/tag refs and deleting remote-only refs |

Empty destination syntax (`src:`), malformed ref names, duplicate destination refs,
and `--mirror` combined with explicit refspecs are rejected before any network write.
Invalid forms return `InvalidRefspec` with exit 129.

### Deletion resolution (`-d`/`--delete` and `:<dst>` refspecs)

Deletion targets are resolved against the refs the remote actually advertises:

- A **short name** tries `refs/heads/<name>` first, then `refs/tags/<name>`; the single
  advertised match is deleted. A name matching both namespaces is refused with
  `dst refspec '<name>' matches more than one remote ref` (Git parity) — qualify the
  target explicitly. Git's broader short-name matching — bare `<name>`, `refs/<name>`,
  `refs/remotes/<name>`, `refs/remotes/<name>/HEAD`, with weak/strong preference — is
  deliberately not implemented; use a fully-qualified target for those namespaces.
- A **fully-qualified name** (`refs/heads/x`, `refs/tags/x`) is used verbatim.
- A deletion naming a ref the remote does not advertise fails with
  `unable to delete '<name>': remote ref does not exist` instead of reporting
  `Everything up-to-date` (issue #465); `--dry-run` reports the same error without
  sending anything. Idempotent cleanup scripts must tolerate this error (git behaves
  the same way).

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

### Pushing to a local repository

`libra push <local-path> <branch>` (or a `file://` URL) pushes into a local
repository (issues/480 HP-07/HP-08). The target is opened by path (no process
cwd switch): for a **Libra** target, missing objects are written to its object
store and refs are compare-and-swap guarded in a single transaction; for a
**Git** target, objects are encoded into a self-contained pack + idx and refs are
updated atomically via `<ref>.lock` + rename. A rejected update (checked-out
branch on a non-bare target, non-fast-forward without `+`/`--force`) leaves the
target untouched.

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
| Force-if-includes | `libra push --force-if-includes` (with the All/Ref lease forms it additionally requires the remote-tracking tip to be integrated locally; silent no-op with the exact lease form or no lease) | `git push --force-if-includes` | N/A |
| Porcelain output | `libra push --porcelain` | `git push --porcelain` | N/A |
| Thin pack | `libra push --thin` (REF_DELTA entries against server-known bases; the self-contained form is the default) | `git push --thin` | N/A |
| Skip pre-push hook | Accepted, no-op | `git push --no-verify` | N/A |
| Suppress progress | `libra push --no-progress` | `git push --no-progress` | N/A |
| Atomic / signed / push-option / follow-tags | `libra push --atomic` / `--signed` (signed push certificate built from the repository signing key: generated or imported) / `-o <opt>` / `--follow-tags` | `git push --atomic` / `--signed` / `-o` / `--follow-tags` | N/A |
| Dry-run | `libra push --dry-run` | `git push --dry-run` | `jj git push --dry-run` |
| Refspec mapping | `libra push origin src:dst` | `git push origin src:dst` | N/A |
| Multiple refspecs | `libra push origin main feature:release` | `git push origin main feature:release` | N/A |
| Delete remote branch | `libra push -d origin branch` or `libra push origin :branch` | `git push -d origin branch` / `git push origin :branch` | `jj git push --delete branch` |
| Delete remote tag by short name | `libra push -d origin tag` (resolves the advertised `refs/tags/<tag>`, ambiguous names refused) | `git push -d origin tag` | N/A |
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
| Configured local upstream (`branch.<name>.remote=.`) | `LBR-CLI-003` | 129 | "use 'libra branch --unset-upstream' to clear the local upstream"; network support is issues/480 HP-16 (Git `push` is 128 — intentional) |
| Invalid refspec | `LBR-CLI-002` | 129 | "use '\<name>' or '\<src>:\<dst>'" |
| Source ref not found | `LBR-CLI-003` | 129 | "verify the local branch/ref exists" |
| Delete target does not exist on remote | `LBR-CLI-003` | 129 | "check the remote's refs with 'libra ls-remote \<remote>'" |
| Ambiguous delete target | `LBR-CLI-003` | 129 | "qualify the target as refs/heads/\<name> or refs/tags/\<name>" |
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
| Unexpected receive-pack status line / missing status-report flush | `LBR-NET-002` | 128 | "check the remote Git service or proxy response and retry" |
| Network error | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| LFS upload failed | `LBR-NET-001` | 128 | "check LFS endpoint configuration" |
| Tracking ref update failed | `LBR-IO-002` | 128 | -- |
| Repository state error | `LBR-REPO-002` | 128 | "try 'libra status' to verify" |

### Timeout Policy

- Discovery / connection: 60s connection timeout
- Upload / receive-pack: 600s idle timeout (no data progress triggers timeout)
- Timeouts are mapped to `NetworkUnavailable` with `phase` detail

## pkt-line protocol errors

Malformed pkt-line frames in HTTP(S) reference discovery, or in receive-pack
status responses over HTTP(S) or SSH, fail with `LBR-NET-002`. An absent HTTP(S) discovery advertisement also
uses `LBR-NET-002`. These errors have a fixed `pkt-line protocol error: ` reason
and do not include the malformed header or payload. Check the remote Git service
and any proxy that may truncate or replace its response, then retry. Other
discovery connectivity failures and transport configuration errors retain
`LBR-NET-001`; authentication and timeout handling retain their existing behavior.

## Receive-pack status reports

An unexpected receive-pack status line returns `LBR-NET-002` (exit 128), with
`pkt-line protocol error: unexpected receive-pack status line`. The diagnostic
does not echo that status line. Every report must reach an explicit `0000`
flush before its unpack/ref statuses are interpreted. An empty response or EOF
before that flush returns `LBR-NET-002` with the fixed reason
`missing receive-pack status flush`, including truncated unpack/`ng` rejections.
Both cases use `check the remote Git service or proxy response and retry`.

Ordinary transport failures retain `LBR-NET-001`. Completely framed server-declared unpack failures
and `ng` ref rejections retain `LBR-NET-002` with their existing server-log or
branch-protection hints; valid `ng` reasons remain visible. Local remote-tracking
refs are updated only after successful status validation. A failed response
does not prove the server rolled back its refs: inspect the remote state before
retrying an update.

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

## Remote push rejection messages

When receive-pack reports `ng <refname> <reason>`, Libra first checks that the
refname is one of the local refs submitted for this push. A rejection for any
other ref fails with `LBR-NET-002` (exit 128) and the fixed reason
`receive-pack rejected an unexpected ref`; the unrecognized name and its reason
are not echoed. Its hint asks you to check the remote Git service or proxy.

For a recognized ref, the remote rejection remains readable. Both its name and
reason use the same sanitizer: Unicode control characters, including C0, DEL,
and C1/CSI, become literal escape text. Each displayed field is limited to 200
Unicode characters after escaping, plus `…` when truncated. An escape sequence
or UTF-8 character is never split, so the visible prefix can be shorter than 200
characters. Ordinary short rejection text is unchanged. These rules apply before
human, JSON, and machine rendering, including the decoded JSON message.

Known-ref rejection still returns `LBR-NET-002` / exit 128 with the existing branch
protection hint. JSON keeps the existing message/hints envelope; a separate
structured reason field is not introduced. Readable remote text is not a trusted
local assertion. A rejected response leaves local tracking refs unchanged; it
does not prove that the server rolled back a partial remote update. Inspect the
remote state before retrying when the server's result is uncertain.

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
