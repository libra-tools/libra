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

When invoked with no arguments, it fetches from the current branch's configured upstream.
When `--all` is given, every configured remote is fetched in sequence. When a specific
`<repository>` is named, only that remote is contacted. An optional `<refspec>` selects
one source ref and may map it to an exact local destination (`<src>:<dst>`). When no
explicit refspec is given, `remote.<name>.fetch` entries are honored; if none exist,
all advertised branches use the default `refs/remotes/<name>/*` mapping.

Fetch supports SSH, HTTPS, local file, and `git://` transports. Vault-backed SSH keys
are loaded automatically when configured via `vault.ssh.<remote>.privkey`.

## Global Config Schema Guard

`libra fetch` reads the global storage configuration (`~/.libra/config.db`, or
`LIBRA_CONFIG_GLOBAL_DB`) before trusting remote/tiered object storage settings. If that
database has a schema version newer than this binary supports, fetch fails closed with
`LBR-CONFIG-001` instead of silently ignoring global storage config and falling back to
local objects. The diagnostic includes the binary path and version, config DB path,
schema versions, and the update command:
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`.

Use `libra --offline fetch ...` or `LIBRA_READ_POLICY=offline|local libra fetch ...` only when
you intentionally want local-only object access. Libra will warn once and ignore the
global storage config for that run.

### Prune config defaults (`fetch.prune`, `remote.<name>.prune`)

When neither `--prune`/`-p` nor `--no-prune` is given, Libra resolves the prune
behavior from Git-compatible config defaults, per remote: `remote.<name>.prune`
first, then `fetch.prune`, each read through the local → global → system cascade
(case-insensitive keys; encrypted local/global values are decrypted; legacy rows
are honored; an unreadable or unsupported system scope is skipped). When neither
key is set the built-in default is `false` — the same shipped default as Git.
CLI flags always win over config.

An invalid value fails closed with `LBR-CLI-002` and an unreadable local/global
scope with `LBR-IO-001`, in both cases **before the fetch touches the network**
(with `--all`, every remote's prune mode is validated before the first fetch),
so a bad config can never produce a fetch whose prune semantics silently
diverge from what was configured. Exception: a global config store whose
schema is newer than this Libra binary is skipped for these defaults with a
one-time deduplicated warning; the dispatch-level guard still fails `fetch`
closed with `LBR-CONFIG-001` when the command genuinely needs global storage
config.

### Fetch refspecs

Short source names such as `main` mean `refs/heads/main` and default to
`refs/remotes/<remote>/main`. Full mappings and the common wildcard form are supported:

```bash
libra fetch origin refs/heads/main:refs/remotes/origin/release
libra config set --add remote.origin.fetch \
  +refs/heads/*:refs/remotes/origin/*
```

Explicit refspecs override configured mappings. `remote add -t` and `remote
set-branches` write concrete `remote.<name>.fetch` values that later fetches now enforce.
Config variable names are case-insensitive, so spellings such as
`remote.origin.Fetch` are honored. Destinations are currently limited to
`refs/heads/*` and `refs/remotes/<remote>/*`; the reserved `HEAD` destination in
either namespace, and every other namespace, fail before any write.
Multiple destination updates, their reflogs, and `refs/remotes/<name>/HEAD` are committed
in one SQLite transaction; any rejected destination rolls back the complete ref update.
Non-fast-forward updates require `+` on that mapping or `--force`. Fetching into the
local branch checked out by any linked worktree is rejected. On a full fetch, a cached
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
| `--depth <N>` | Limit fetching to the specified number of commits from the tip of each remote branch (shallow fetch). Supported for Git remotes that advertise shallow boundaries. Local Libra remotes fail closed with `LBR-REPO-002` — the accepted end state (decision D20), since that transport cannot advertise shallow metadata. | `libra fetch origin --depth 1` |
| `--tags` | Fetch every tag from the remote into the local `refs/tags/*` (overrides the default auto-follow and `remote.<name>.tagOpt`). | `libra fetch origin --tags` |
| `--no-tags` | Fetch no tags at all, not even tags reachable from fetched commits (overrides the default auto-follow). | `libra fetch origin --no-tags` |
| `--no-auto-gc` | Do not run a repacking/gc pass after fetching. Accepted no-op for Git parity: Libra's fetch never triggers an automatic gc, so there is nothing to disable. | `libra fetch origin --no-auto-gc` |
| `--no-progress` | Do not show the progress meter (the "Receiving objects" spinner / remote progress) on stderr, matching `git fetch --no-progress`. | `libra fetch origin --no-progress` |
| `-p`, `--prune` | After the fetch, delete remote-tracking refs under `refs/remotes/<remote>/*` that are not live destinations of the effective configured refspec mapping. A one-off explicit refspec retains the configured mapped destinations, ordinary advertised scope, and its selected destination. Deletions plus an audit reflog entry run in one transaction. Local branches, tags, `refs/remotes/<remote>/HEAD`, and other remotes are never touched. With `--dry-run`, stale refs are reported but not deleted. Overrides the `remote.<name>.prune` / `fetch.prune` config defaults. | `libra fetch origin -p` |
| `--no-prune` | Do not prune remote-tracking refs, overriding the `remote.<name>.prune` / `fetch.prune` config defaults (the built-in default is no pruning). `--prune`/`--no-prune` form a last-one-wins toggle: when both are given, the last on the command line wins (Git semantics). | `libra fetch origin --no-prune` |
| `--notes` | Also import the file-dependency graph (`refs/notes/deps`, lore.md 3.2) from the remote over a dedicated side-channel. Default OFF (Git never auto-fetches notes). v1 travels notes only from a **local Libra source**; a network or plain-Git remote emits an honest "not supported yet" warning and imports no graph (deferred, D17). Import union-merges into any local edges and re-validates every endpoint, and is per-note fault-tolerant (a malformed note, or one whose commit is absent locally, is skipped with a warning, never aborting the fetch). Persist the opt-in per remote with `remote.<name>.fetchNotesDeps=true`. | `libra fetch origin --notes` |
| `-f`, `--force` | Allow non-fast-forward updates and overwrite (clobber) a local tag that points elsewhere. Forced updates are marked `+` in `--porcelain` / `(forced update)` in human output. | `libra fetch origin --tags --force` |
| `--dry-run` | Preview the remote-tracking ref updates the fetch would produce without downloading any objects or writing refs, reflog, or `FETCH_HEAD`. | `libra fetch origin --dry-run` |
| `--append` | Append fetched ref records to `.libra/FETCH_HEAD` instead of overwriting it. (`-a` is reserved for `--all`.) | `libra fetch origin --append` |
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
- A full-history fetch followed by `fetch --depth N` is idempotent.
- Re-fetching an already-shallow repository at the same depth is also
  idempotent: Libra persists server-advertised shallow boundaries in
  `.libra/shallow` and sends them during later upload-pack negotiation.
- Sparse checkout (`clone --sparse`) is **not** part of this contract — see
  [`docs/development/commands/_compatibility.md`](../development/commands/_compatibility.md)
  for why sparse-checkout is intentionally deferred.

Shallow fetch does introduce the usual Git "shallow boundary" caveats (blame,
log, merge-base computation may not see commits beyond the boundary). That
trade-off is a user-visible knob, not a default — full-history fetch remains
the default and the recommended posture for monorepo and AI-agent workflows.
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
| Remote branch not found | `LBR-CLI-003` | 129 | "verify the remote branch name and try again" |
| Invalid/mismatched fetch refspec | `LBR-CLI-002` | 129 | Use a valid `<src>:<dst>` mapping with matching optional wildcards |
| Configured refspec read failure | `LBR-IO-001` | 128 | Inspect `remote.<name>.fetch` configuration |
| Checked-out destination / non-fast-forward without force | `LBR-CONFLICT-002` | 128 | Change the destination, add `+`, or use `--force` intentionally |
| Invalid remote spec (missing repo, malformed URL, unsupported scheme) | `LBR-CLI-003` or `LBR-REPO-001` | 129 / 128 | Varies by cause |
| Authentication failure during discovery | `LBR-AUTH-002` | 128 | "check SSH key / HTTP credentials and repository access rights" |
| Network timeout / transport failure | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| Packet / sideband / checksum / pack protocol failure | `LBR-NET-002` | 128 | "the remote did not respond correctly" |
| Object format mismatch | `LBR-REPO-003` | 128 | "remote uses a different hash algorithm" |
| Failed to create pack directory | `LBR-IO-002` | 128 | "check filesystem permissions" |
| Failed to write pack/index/refs | `LBR-IO-002` | 128 | "check filesystem permissions and disk space" |
| Local state corruption | `LBR-REPO-002` | 128 | "inspect repository state and object integrity" |
