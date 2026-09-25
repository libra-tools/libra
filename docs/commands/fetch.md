# `libra fetch`

Download objects and update remote-tracking refs from another repository.

## Synopsis

```
libra fetch [OPTIONS] [<repository> [<refspec>]]
```

## Description

`libra fetch` contacts a remote repository, negotiates which objects the local store is
missing, downloads them as a pack file, indexes the pack, and updates the corresponding
remote-tracking refs (e.g. `refs/remotes/origin/main`). It never modifies the working
tree or the current branch -- use `libra pull` or `libra merge` for that.

When invoked with no arguments, it fetches from the current branch's configured upstream. A configured local upstream (`branch.<name>.remote=.`) is refused before any network or `FETCH_HEAD` write (`LBR-CLI-003`, exit 129); Git 2.54 `fetch` can operate on a local upstream — that support is deferred to [issues/480 HP-16](https://github.com/libra-tools/libra/issues/480). An explicit repository argument `.` keeps the existing `remote '.' not found` path.
When `--all` is given, every configured remote is fetched in sequence. When a specific
`<repository>` is named, only that remote is contacted. An optional `<refspec>` selects
one source ref and may map it to an exact local destination (`<src>:<dst>`). When no
explicit refspec is given, `remote.<name>.fetch` entries are honored; if none exist,
all advertised branches use the default `refs/remotes/<name>/*` mapping.

A `<repository>` may also be an anonymous local repository spec — a `file://` URL,
absolute path, or relative path — which fetches into `FETCH_HEAD` only (no tracking
ref, no `remote.*` config), matching Git's unnamed-remote transport. A configured
remote name always wins over a same-named directory. A requested remote ref that
does not exist reports `couldn't find remote ref <name>`, matching Git.

Fetch supports SSH, HTTPS, local file, Git v2 bundle files, and `git://`
transports. A remote URL that points at a bundle is re-read on every fetch
(including `--prune` and `--dry-run`). When `remote.<name>.fetch` is
`+refs/*:refs/*` (a `--mirror` clone), fetch updates those refs in place and
`--prune` deletes mirrored refs the source no longer advertises. Vault-backed
SSH keys are loaded automatically when configured via `vault.ssh.<remote>.privkey`.

## Global Config Schema Guard

Configuration schema compatibility is role-scoped. Before `libra fetch` trusts
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

### Prune config defaults (`fetch.prune`, `remote.<name>.prune`)

When neither `--prune`/`-p` nor `--no-prune` is given, Libra resolves the prune
behavior from Git-compatible config defaults, per remote: `remote.<name>.prune`
first, then `fetch.prune`, each read through the local → global → system cascade
(case-insensitive keys; encrypted local/global values are decrypted; legacy rows
are honored; an unreadable system scope is skipped). When neither
key is set the built-in default is `false` — the same shipped default as Git.
CLI flags always win over config.

An invalid value fails closed with `LBR-CLI-002` and an unreadable local/global
scope with `LBR-IO-001`, in both cases **before the fetch touches the network**
(with `--all`, every remote's prune mode is validated before the first fetch),
so a bad config can never produce a fetch whose prune semantics silently
diverge from what was configured. An unsupported global schema is skipped for
these defaults with one deduplicated warning only when dispatch proves Global
storage config unnecessary. The dispatch guard fails `fetch` closed with
`LBR-CONFIG-001` when it requires unsupported Global config, or when System
has a future/unregistered receipt; Global credential overrides do not bypass
System defaults. Known Repository receipts and valid barriers remain readable.

### Fetch refspecs

Short source names such as `main` mean `refs/heads/main` and default to
`refs/remotes/<remote>/main`. Full mappings and the common wildcard form are supported:

```bash
libra fetch origin refs/heads/main:refs/remotes/origin/release
libra config set --add remote.origin.fetch \
  +refs/heads/*:refs/remotes/origin/*
```

Explicit refspecs override configured mappings. A bare source such as `fetch origin dev`
still downloads that ref and records it in `FETCH_HEAD`, but when `remote.<name>.fetch`
is already set and does not map `dev`, no new remote-tracking branch is created
(matching a single-branch clone; issues/474 CL-05). `remote add -t` and `remote
set-branches` write concrete `remote.<name>.fetch` values that later fetches now enforce.
Config variable names are case-insensitive, so spellings such as
`remote.origin.Fetch` are honored. Destinations are currently limited to
`refs/heads/*` and `refs/remotes/<remote>/*` (reserved `HEAD` is refused);
`+refs/*:refs/*` mirror refspecs also map other legal `refs/*` names.
Multiple destination updates, their reflogs, and `refs/remotes/<name>/HEAD` are committed
in one SQLite transaction; any rejected destination rolls back the complete ref update.
Non-fast-forward updates require `+` on that mapping or `--force`. Fetching into the
local branch checked out by any linked worktree is rejected (bare repositories with
`core.bare=true` skip that check). On a full fetch, a cached
remote HEAD is removed when the effective mapping no longer includes the remote's default
source branch. Tag destinations remain controlled by `--tags` / `--no-tags`, not fetch
refspec mappings.

```bash
libra config fetch.prune true           # prune on every fetch
libra config remote.origin.prune false  # but never for origin
```

## Options

| Flag / Argument | Description | Example |
|-----------------|-------------|---------|
| `<repository>` | Remote name or URL to fetch from. When omitted, uses the current branch's upstream remote. | `libra fetch origin` |
| `<refspec>` | Source ref or exact `<src>:<dst>` mapping. Requires `<repository>`. When omitted, `remote.<name>.fetch` mappings are used, falling back to all remote branches. | `libra fetch origin refs/heads/main:refs/remotes/origin/release` |
| `-a`, `--all` | Fetch from every configured remote. Conflicts with `<repository>`. | `libra fetch --all` |
| `--depth <N>` | Limit fetching to the specified number of commits from the tip of each remote branch (shallow fetch). Network Git (`git://`), HTTP(S), and SSH servers must advertise the `shallow` capability; an in-process local Git path supports `--depth` without such an advertisement. Local Libra remotes fail closed with `LBR-REPO-002` — the accepted end state (decision D20), since that transport cannot provide shallow metadata. | `libra fetch origin --depth 1` |
| `--unshallow` | Convert a shallow repository to a complete one: fetch the full history and drop the shallow boundary records. A repository without shallow history errors, and a local Libra source is refused (D20). | `libra fetch --unshallow origin main` |
| `--negotiation-tip <commit>` | Restrict the negotiation `have` set to commits reachable from the given commit or ref (repeatable). Accepted by the local transport, which computes the object difference from the narrowed set. A missing/unresolvable tip errors. | `libra fetch --negotiation-tip <oid> origin main` |
| `--tags` | Fetch every tag from the remote into the local `refs/tags/*` (overrides the default auto-follow and `remote.<name>.tagOpt`). | `libra fetch origin --tags` |
| `--no-tags` | Fetch no tags at all, not even tags reachable from fetched commits (overrides the default auto-follow). | `libra fetch origin --no-tags` |
| `--no-auto-gc` | Do not run a repacking/gc pass after fetching. Accepted no-op for Git parity: Libra's fetch never triggers an automatic gc, so there is nothing to disable. | `libra fetch origin --no-auto-gc` |
| `--no-progress` | Do not show the progress meter (the "Receiving objects" spinner / remote progress) on stderr, matching `git fetch --no-progress`. | `libra fetch origin --no-progress` |
| `-p`, `--prune` | After the fetch, delete remote-tracking refs under `refs/remotes/<remote>/*` that are not live destinations of the effective configured refspec mapping. On a `--mirror` remote (`+refs/*:refs/*`), delete mirrored refs the source no longer advertises (locked short names such as `main` are skipped). A one-off explicit refspec retains the configured mapped destinations, ordinary advertised scope, and its selected destination. Deletions plus an audit reflog entry run in one transaction. Local branches, tags, `refs/remotes/<remote>/HEAD`, and other remotes are never touched on a non-mirror fetch. With `--dry-run`, stale refs are reported but not deleted. Overrides the `remote.<name>.prune` / `fetch.prune` config defaults. | `libra fetch origin -p` |
| `-P`, `--prune-tags` | Prune local tags the remote no longer advertises. Only effective together with `--prune` (or `--mirror`) and ignored when an explicit refspec is given; `--prune-tags` alone does nothing (Git parity). Honors `remote.<name>.pruneTags` / `fetch.pruneTags` config defaults when `--prune-tags` is not given. | `libra fetch origin --prune --prune-tags` |
| `--atomic` | Update all fetched refs atomically: any rejected (non-fast-forward) update rolls back every ref, reflog, and `FETCH_HEAD` write. Libra's fetch already updates refs in a single transaction, so this is accepted for Git parity and asserts the all-or-nothing behaviour. | `libra fetch --atomic origin` |
| `--no-prune` | Do not prune remote-tracking refs, overriding the `remote.<name>.prune` / `fetch.prune` config defaults (the built-in default is no pruning). `--prune`/`--no-prune` form a last-one-wins toggle: when both are given, the last on the command line wins (Git semantics). | `libra fetch origin --no-prune` |
| `--notes` | Also import the file-dependency graph (`refs/notes/deps`, lore.md 3.2) from the remote over a dedicated side-channel. Default OFF (Git never auto-fetches notes). v1 travels notes only from a **local Libra source**; a network or plain-Git remote emits an honest "not supported yet" warning and imports no graph (deferred, D17). Import union-merges into any local edges and re-validates every endpoint, and is per-note fault-tolerant (a malformed note, or one whose commit is absent locally, is skipped with a warning, never aborting the fetch). Persist the opt-in per remote with `remote.<name>.fetchNotesDeps=true`. | `libra fetch origin --notes` |
| `-f`, `--force` | Allow non-fast-forward updates and overwrite (clobber) a local tag that points elsewhere. Forced updates are marked `+` in `--porcelain` / `(forced update)` in human output. | `libra fetch origin --tags --force` |
| `--dry-run` | Preview the remote-tracking ref updates the fetch would produce without downloading any objects or writing refs, reflog, or `FETCH_HEAD`. | `libra fetch origin --dry-run` |
| `--append` | Append fetched ref records to `.libra/FETCH_HEAD` instead of overwriting it. (`-a` is reserved for `--all`.) | `libra fetch origin --append` |
| `--set-upstream` | After a successful single-branch fetch from a named remote, record the current branch's upstream (`branch.<name>.remote` / `branch.<name>.merge`). A colon refspec (`src:dst`) or no branch argument writes nothing (Git warns for the colon form). | `libra fetch --set-upstream origin main` |
| `--update-head-ok` | Allow an explicit refspec to update the currently checked-out branch (with `+` as needed for non-fast-forward). Without it, fetching into the checked-out branch is refused. | `libra fetch --update-head-ok origin master:master` |
| `--refmap=<spec>` | Replace the configured `remote.<name>.fetch` mapping used to derive the tracking destination for a command-line refspec. An empty value (`--refmap=`) updates no tracking ref (FETCH_HEAD only). Requires a command-line refspec. | `libra fetch --refmap= origin main` |
| `-v`, `--verbose` | Announce the remote being contacted on stderr; the stdout result contract is unchanged. | `libra fetch origin -v` |
| `--porcelain` | Print a machine-readable `<flag> <old-oid> <new-oid> <local-ref>` line per ref update. Mutually exclusive with `--json`. | `libra fetch origin --porcelain` |
| `--json` | Emit structured JSON envelope to stdout (global flag). | `libra --json fetch origin` |
| `--machine` | Compact single-line JSON; suppresses progress (global flag). | `libra --machine fetch origin` |
| `--progress none` | Suppress NDJSON progress events on stderr in JSON mode. | `libra --json fetch origin --progress none` |
| `--quiet` | Suppress human-readable output. | `libra fetch --quiet` |

## Common Commands

```bash
libra fetch
libra fetch origin
libra fetch origin main
libra fetch origin refs/heads/main:refs/remotes/origin/release
libra fetch --all
libra fetch origin --depth 1               # shallow fetch
libra fetch origin --tags                  # also fetch all tags into refs/tags/*
libra fetch --all --depth 3                # shallow across all remotes
libra fetch origin --dry-run               # preview ref updates, write nothing
libra fetch origin --porcelain             # machine-readable per-ref lines
libra fetch origin -v                      # announce the remote on stderr
libra fetch origin --append                # accumulate into FETCH_HEAD
libra --json fetch origin
libra --json fetch origin --progress none
```

## Network timeouts

A network fetch (`http(s)://`, `git://`, `ssh://`) is bounded by these timeouts
so a dead or black-holed remote cannot hang the command forever:

| Timeout | Default | What it bounds |
|---------|---------|----------------|
| connect | 30s | the TCP (+ TLS) handshake when opening the connection |
| idle    | 60s | the longest gap with no bytes arriving during ref advertisement or pack streaming (it resets whenever data arrives, so a slow-but-steady transfer is not cut off) |
| first-byte | 30s | the wait from sending the `want` list to the first response byte (`NAK` / pack header) — catches a server that accepts the negotiation but never starts streaming, sooner than the idle timeout would. Applied to `git://`; `http(s)`/`ssh` bound the first response through their own read timeouts |

Each is resolved in this precedence order:

1. an environment variable in milliseconds — `LIBRA_FETCH_CONNECT_TIMEOUT_MS`,
   `LIBRA_FETCH_IDLE_TIMEOUT_MS`, `LIBRA_FETCH_FIRST_BYTE_TIMEOUT_MS`;
2. a config value in whole seconds — `fetch.<remote>.connectTimeout` /
   `fetch.<remote>.idleTimeout` / `fetch.<remote>.firstByteTimeout`, then the
   un-scoped `fetch.connectTimeout` / `fetch.idleTimeout` / `fetch.firstByteTimeout`;
3. the built-in default above.

```
# Give a flaky remote longer to connect, for this remote only.
libra config fetch.origin.connectTimeout 90

# One-off override (milliseconds) without touching config.
LIBRA_FETCH_IDLE_TIMEOUT_MS=120000 libra fetch origin
```

Local (`file://` / path) remotes read from disk and are not subject to network
timeouts. `git://` connections are now bounded by all three timeouts (previously
they had none). An unparseable env/config value is ignored rather than applied,
so a typo never leaves a fetch with a zero or nonsensical timeout.

## Shallow Fetch Integrity

`--depth <N>` is accepted only when the selected transport can return shallow
boundary metadata. Local Git repositories and network Git remotes can do this.
A local Git remote uses the same shortest-distance union as clone: a commit is
a shallow boundary when a parent was not sent, or when a root sits exactly on
the depth cutoff (issues/474 CL-04).
Git servers reached through `git://`, HTTP(S), or SSH can also advertise existing
`shallow <oid>` boundaries without `--depth`. Fetch records an advertised
boundary in `.libra/shallow` only when that commit exists locally and a parent
is missing. Git-protocol clients request `shallow` only if the server advertises
the capability; one advertisement may contain at most 4,096 distinct boundaries.
An upload-pack response separately accepts at most 4,096 distinct OIDs across
its `shallow` and `unshallow` boundary lines.
Those response lines are capped at 8,192 in total, including duplicates; OIDs
are checked against the server's object format, with violations returning
`LBR-NET-002`.
Inspecting advertised boundary commits is limited to 4 MiB of decoded payload
per commit, 64 MiB of decoded commit payload per fetch, and 262,144 parent IDs
in total. Exceeding a limit aborts the fetch; aggregate-limit errors suggest
fetching fewer refs or asking the remote owner to reduce its shallow boundaries.
Network Git (`git://`), HTTP(S), and SSH fetches verify wanted objects and
fetched commit-parent links against final shallow boundaries before updating
refs; an unmarked missing parent fails the fetch. These checks cap the temporary
parent-edge spool at 1 GiB and the pack's temporary commit-ID buffer at 64 MiB
(currently up to 2,097,152 commits) per fetch. Either resource limit can reject
an otherwise valid large pack before refs are updated; it does not imply pack
corruption.
When depth-response shallow markers need further validation, at most 16,384
requested objects or tag targets are inspected. The shallow-marker ancestry
walk separately caps visited commits and parent edges at 262,144 each. Remote
type probes and inspected tags share a 256 MiB decoded object-payload budget
across each shallow response validation. If a limit is exceeded, split the
fetch or reduce the selected refs.
Smart HTTP additionally checks the advertisement again after the upload-pack
POST; if the boundaries changed, it reports a `NetworkProtocol` error asking you
to retry before writing the pack or refs.

Local Libra repositories cannot (the accepted end state — decision D20 in the
development compatibility register), so `libra fetch <local-libra-remote>
--depth <N>` fails before downloading objects or writing `.libra/shallow`,
classified as `LBR-REPO-002`. This fail-closed behavior prevents a remote-tracking
ref from pointing at a commit whose parents are missing without a shallow marker.

## FETCH_HEAD

Every successful fetch records the fetched refs in `.libra/FETCH_HEAD`, one
`<oid>\tnot-for-merge\tbranch '<name>' of <url>` line per branch and one
`<oid>\tnot-for-merge\ttag '<name>' of <url>` line per fetched tag. Libra never
designates a merge target (merge with `libra pull`), so every line is marked
`not-for-merge`. `--append` accumulates into the file instead of overwriting it;
`--dry-run` writes nothing. Selected refs are recorded even when their local destination
was already up to date. Plain fetch does not create or modify `ORIG_HEAD`.

## Human Output

Successful human mode prints a compact summary:

```text
From /path/to/remote.git
 * [new ref]         origin/main
 32 objects fetched
```

When nothing changed:

```text
From /path/to/remote.git
Already up to date with 'origin'
```

## Structured Output (JSON examples)

- `--json` writes one success envelope to `stdout`
- `--machine` writes the same schema as compact single-line JSON
- `stdout` is reserved for the final envelope only

### Top-Level Schema

- `all`: whether `--all` was used
- `requested_remote`: explicit remote name, or `null` for `--all`
- `refspec`: requested branch/refspec when provided
- `remotes[]`: per-remote fetch results

### Per-Remote Result Schema

- `remote`: logical remote name
- `url`: normalized remote URL/path
- `refs_updated[]`: local destination refs that changed
- `objects_fetched`: object count parsed from the received pack
- `bytes_received`: byte size of the received pack stream (0 when nothing was transferred)
- `pruned[]`: stale remote-tracking refs removed by pruning (`{remote_ref, branch, old_oid}`); present only when pruning removed at least one ref

### Refs Updated Schema

- `remote_ref`: fully qualified local destination ref, e.g. `refs/remotes/origin/main`
- `old_oid`: previous object id, or `null` when the ref is new
- `new_oid`: fetched object id
- `forced`: `true` when the update was not a fast-forward and was allowed by a leading `+` mapping or `--force`, or when a tag was clobbered under `--force`

Example (single remote):

```json
{
  "ok": true,
  "command": "fetch",
  "data": {
    "all": false,
    "requested_remote": "origin",
    "refspec": null,
    "remotes": [
      {
        "remote": "origin",
        "url": "git@github.com:user/repo.git",
        "refs_updated": [
          {
            "remote_ref": "refs/remotes/origin/main",
            "old_oid": "abc1234...",
            "new_oid": "def5678...",
            "forced": false
          }
        ],
        "objects_fetched": 32,
        "bytes_received": 4096
      }
    ]
  }
}
```

Example (already up to date):

```json
{
  "ok": true,
  "command": "fetch",
  "data": {
    "all": false,
    "requested_remote": "origin",
    "refspec": null,
    "remotes": [
      {
        "remote": "origin",
        "url": "git@github.com:user/repo.git",
        "refs_updated": [],
        "objects_fetched": 0,
        "bytes_received": 0
      }
    ]
  }
}
```

## Progress

- In `--json` mode, progress defaults to NDJSON events on `stderr`
- Use `--progress none` to keep `stderr` quiet in JSON mode
- `--machine` disables progress automatically and keeps `stderr` clean on success

## Design Rationale

### Pruning is opt-in, not the default

Git's shipped default is also `fetch.prune = false`, though enabling it is a commonly
recommended setting because stale remote-tracking refs accumulate silently. Libra keeps
the same shipped default — no pruning — for two additional reasons: (1) in
agent-driven workflows, stale tracking refs can serve as useful historical anchors for
diffing against a previous remote state, and (2) destructive ref cleanup should be a
deliberate choice. Pruning is opt-in via `--prune`/`-p`, the `fetch.prune` /
`remote.<name>.prune` config defaults above, or the standalone
`libra remote prune <name>`. `--no-prune` is the built-in default; `--prune`/`--no-prune`
form a last-one-wins toggle and always override the config, matching Git.

When pruning is enabled (flag or config), after the fetch completes Libra removes every
`refs/remotes/<remote>/*` ref that is not a live destination of the effective configured
refspec mapping, using the same destination-aware rule as `remote prune`. A one-off
explicit refspec preserves configured mapped destinations, the ordinary full-remote
advertised scope, and its selected destination. The deletions and a non-lossy audit reflog entry (`<old> -> 0…0`) run in a
single transaction, so a mid-prune failure rolls back every deletion. `--dry-run` reports
the stale refs without writing. Pruning is skipped entirely when the remote advertises no
refs at all (so a transient empty advertisement cannot wipe every tracking ref), and
pruned refs never appear in `FETCH_HEAD` (which records only fetched refs).

### Shallow fetch (`--depth`) is exposed as a stable flag

`libra fetch --depth N` is a public stable flag (audited C3 in
[`docs/development/commands/clone.md`](../development/commands/clone.md)).
The internal `fetch_repository(..., depth)` plumbing has supported shallow fetch
for some time; C3 surfaces it on the CLI and binds the contract:

- `--depth N` limits fetching to the latest `N` commits per remote branch.
- It composes with `--all`: a shallow fetch across all configured remotes is
  `libra fetch --all --depth N`.
- `fetch --depth N` can add shallow boundaries to a complete repository.
  Repeating it at the same depth against unchanged remote refs is idempotent:
  Libra persists server-advertised boundaries in `.libra/shallow` and sends
  them during later upload-pack negotiation.
- Sparse checkout (`clone --sparse`) is **not** part of this contract — see
  [`docs/development/commands/_compatibility.md`](../development/commands/_compatibility.md)
  for why sparse-checkout is intentionally deferred.

Shallow fetch does introduce the usual Git "shallow boundary" caveats (blame,
log, merge-base computation may not see commits beyond the boundary). That
trade-off can be requested with `--depth`; without it, fetch requests all history
available from the source, which may itself be shallow. Full history remains
the recommended posture for monorepo and AI-agent workflows.
Tiered cloud storage (S3/R2 + LRU caching) remains the bandwidth solution for
the cases where full history is wanted.

### Why JSON progress on stderr?

Structured progress events (object counts, bytes received) are emitted as NDJSON lines
on stderr so that agent frameworks can parse real-time progress without interfering with
the final result envelope on stdout. This follows the Unix convention of separating status
information (stderr) from data output (stdout). The `--progress none` flag allows callers
that do not need progress to suppress it entirely, and `--machine` mode disables progress
by default for maximum script friendliness.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Fetch upstream | `libra fetch` | `git fetch` | `jj git fetch` |
| Named remote | `libra fetch origin` | `git fetch origin` | `jj git fetch --remote origin` |
| Single branch | `libra fetch origin main` | `git fetch origin main` | `jj git fetch --remote origin --branch main` |
| Exact ref mapping | `libra fetch origin <src>:<dst>` | `git fetch origin <src>:<dst>` | Not supported |
| Configured mappings | `remote.<name>.fetch` (including one `*` wildcard per side) | Same | Not supported |
| All remotes | `libra fetch --all` | `git fetch --all` | `jj git fetch --all-remotes` |
| Prune stale refs | `libra fetch -p` / `fetch.prune`, `remote.<name>.prune` config / `libra remote prune <name>` | `git fetch --prune` / same config keys | Automatic |
| Shallow fetch | `libra fetch --depth N` | `git fetch --depth N` | Not supported |
| Dry-run preview | `libra fetch --dry-run` | `git fetch --dry-run` | Not supported |
| Porcelain output | `libra fetch --porcelain` | `git fetch --porcelain` | No |
| Append FETCH_HEAD | `libra fetch --append` | `git fetch --append` | No |
| Verbose diagnostics | `libra fetch -v` | `git fetch -v` | No |
| Tag auto-follow (default) | Tags reachable from fetched commits are followed automatically (via `include-tag`) | Same (default) | Automatic |
| Tag fetch control | `libra fetch --tags` / `--no-tags`; `remote.<name>.tagOpt` | `git fetch --tags` / `--no-tags`; `remote.<name>.tagOpt` | Automatic |
| Force fetch | `libra fetch -f` / `--force` (non-FF + tag clobber) | `git fetch --force` | Automatic |
| Atomic / refmap | Not supported (deferred) | `git fetch --atomic` / `--refmap` | No |
| Structured output | `--json` / `--machine` | No | No |
| Progress events | NDJSON on stderr | Text on stderr | Text on stderr |

## Error Handling

| Scenario | StableErrorCode | Exit | Hint |
|----------|-----------------|------|------|
| No configured upstream / detached HEAD | `LBR-REPO-003` | 128 | "checkout a branch or specify a remote" |
| Remote not found | `LBR-CLI-003` | 129 | "use 'libra remote -v' to see configured remotes" |
| Configured local upstream (`branch.<name>.remote=.`) | `LBR-CLI-003` | 129 | "use 'libra branch --unset-upstream' to clear the local upstream"; network support is issues/480 HP-16 |
| Remote branch not found | `LBR-CLI-003` | 129 | "verify the remote branch name and try again" |
| Invalid/mismatched fetch refspec | `LBR-CLI-002` | 129 | Use a valid `<src>:<dst>` mapping with matching optional wildcards |
| Configured refspec read failure | `LBR-IO-001` | 128 | Inspect `remote.<name>.fetch` configuration |
| Checked-out destination / non-fast-forward without force | `LBR-CONFLICT-002` | 128 | Change the destination, add `+`, or use `--force` intentionally |
| Invalid remote spec (missing repo, malformed URL, unsupported scheme) | `LBR-CLI-003` or `LBR-REPO-001` | 129 / 128 | Varies by cause |
| Authentication failure during discovery | `LBR-AUTH-002` | 128 | "check SSH key / HTTP credentials and repository access rights" |
| Network timeout / transport failure | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| pkt-line discovery / transfer setup error / empty advertisement | `LBR-NET-002` | 128 | "check that the remote serves Git data and that a proxy has not altered the response" |
| Packet-read connection reset / non-protocol IO failure | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| pkt-line truncation / sideband / checksum / pack protocol failure | `LBR-NET-002` | 128 | No additional hint, except for an incomplete pack: "the connection dropped mid-transfer — retry the fetch" |
| Object format mismatch | `LBR-REPO-003` | 128 | "remote uses a different hash algorithm" |
| Failed to create pack directory | `LBR-IO-002` | 128 | "check filesystem permissions" |
| Failed to write pack/index/refs | `LBR-IO-002` | 128 | "check filesystem permissions and disk space" |
| Local state corruption | `LBR-REPO-002` | 128 | "inspect repository state and object integrity" |

## Truncated packets during fetch

Fetch reports `LBR-NET-002` for truncation of a received pkt-line header or payload
midway through the frame. These errors contain a fixed reason without the
remote bytes. Lengths from one to three are rejected before allocating a payload;
flush (`0000`), empty data (`0004`) and maximum-length (`ffff`) frames remain valid.
Header decoding errors also use fixed reasons without echoing the received bytes.

If a transfer stops inside a packet before the pack is complete, the error
reports packet truncation without a received-byte count or an extra CLI hint.
Ending between packets with an incomplete pack still reports the byte count and
adds the hint "the connection dropped mid-transfer — retry the fetch".

A complete, checksum-verified pack can still finish without a flush or connection
close. Fetch also checks trailing packet bytes already available at completion;
a partial frame in those bytes is an error. Check whether the connection or a
proxy truncated the response, then retry the fetch.
For network remotes, if a trailing frame starts but then stalls, the existing
transport idle timeout applies; timing out fails the fetch with `LBR-NET-001`.

## Malformed HTTP(S) discovery responses

During HTTP(S) reference discovery, Libra rejects a zero-byte advertisement and
malformed pkt-line frames, including short or non-hexadecimal headers, frame
lengths below four, and truncated payloads. A valid `0000` flush remains distinct
from an absent response; a valid empty-repository advertisement is supported.
An unsupported object-format capability reports the fixed message
`Unsupported object format capability` without echoing its remote value.
Check that the URL points to a Git smart HTTP service and that a proxy has not
truncated or replaced the response; then retry.

Fetch discovery reports `LBR-NET-002` for an empty advertisement or malformed
pkt-line response, without echoing its header or payload bytes. Ordinary network
failures retain `LBR-NET-001`; verify the Git service and any proxy response
before retrying a protocol failure.

## pkt-line error classification

Detected pkt-line framing errors return `LBR-NET-002` (exit 128), including an
empty HTTP(S) discovery advertisement. Ordinary connection failures, resets and
timeouts return `LBR-NET-001` (exit 128). Verify the Git service and any proxy
response when a protocol error occurs. Discovery framing errors use the hint
`check that the remote serves Git data and that a proxy has not altered the response`.

Object-transfer setup reports detected pkt-line errors with the same protocol
hint. A truncated header or payload while reading the fetch stream retains
`LBR-NET-002` with no extra CLI hint. An incomplete pack ending at a clean frame
boundary retains its byte count and `the connection dropped mid-transfer — retry
the fetch` hint. A transport reset while reading a packet is `LBR-NET-001`.

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
