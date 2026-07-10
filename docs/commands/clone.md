# `libra clone`

Clone a repository into a new directory.

## Synopsis

```
libra clone [OPTIONS] <REMOTE_REPO> [LOCAL_PATH]
```

## Description

`libra clone` creates a local copy of a remote repository by fetching objects, configuring
`origin`, and checking out the working tree. It initializes a vault-backed repository and
transparently reuses `run_init()` for the local metadata setup.

Cloning fetches all objects and refs from the remote, creates a `.libra` directory with a
SQLite-backed metadata store, sets up the `origin` remote, and checks out the default branch
(or the branch specified with `-b`). Vault signing is always bootstrapped during clone,
matching `libra init` defaults. For non-bare clones, any checked-out `.gitignore` files are
copied to matching `.libraignore` files so Libra ignore rules work immediately.

For bare clones, no working tree checkout is performed and the repository directory itself
becomes the object store. Bare clones do not create `.libraignore`.

## Global Config Schema Guard

`libra clone` reads the global storage configuration (`~/.libra/config.db`, or
`LIBRA_CONFIG_GLOBAL_DB`) before trusting remote/tiered object storage settings. If that
database has a schema version newer than this binary supports, clone fails closed with
`LBR-CONFIG-001` instead of silently ignoring global storage config and falling back to
local objects. The diagnostic includes the binary path and version, config DB path,
schema versions, and the update command:
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`.

Use `libra --offline clone ...` or `LIBRA_READ_POLICY=offline|local libra clone ...` only when
you intentionally want local-only object access. Libra will warn once and ignore the
global storage config for that run.

## Options

### `<REMOTE_REPO>` (required)

The remote repository URL to clone from. Supports SSH (`git@host:user/repo.git`) and
HTTPS (`https://host/user/repo.git`) protocols, as well as local filesystem paths.
`libra+cloud://` publish sources are recognized and strictly validated. The clone
domain must be configured locally before restore starts; otherwise Libra returns
`LBR-AUTH-001` and does not create the destination directory. Configured cloud
sources resolve the D1 site, repository row, published refs, selected/default
revision, object index, and R2 object availability before creating the target
directory. Restore then initializes a local Libra repo, downloads indexed Git
objects from R2, restores refs metadata, writes origin cloud config, and checks
out the selected/default revision. Cloud sources never fall through to generic
Git discovery.

```bash
libra clone git@github.com:user/repo.git
libra clone https://github.com/user/repo.git
libra clone /path/to/local/repo
libra clone libra+cloud://code.example.com/kepler-ledger
libra clone libra+cloud://code.example.com/repo/rp_8f4c1b
libra clone "libra+cloud://code.example.com/kepler-ledger?ref=refs/tags/v1.0.0"
libra clone "libra+cloud://code.example.com/kepler-ledger?revision=latest"
```

For `libra+cloud://`, the authority is the configured clone domain. The path must be
either `/<slug>` or `/repo/<repo_id>`. Only one selector is allowed: `?ref=<branch|tag|full-ref>`
or `?revision=<oid|latest>`.
The first Cloudflare restore surface does not accept Git transport shaping flags:
`--branch`, `--depth`, `--single-branch`, `--bare`, `--mirror`, `--filter`,
`--shallow-since`, and `--shallow-exclude` return `LBR-CLI-002`
before clone-domain config lookup and before creating the destination directory.
Use `?ref=<branch|tag|full-ref>` on the source URL to select a checkout target.

Required clone-domain config keys:

```text
cloud.clone_domains.<domain>.account_id
cloud.clone_domains.<domain>.d1_database_id
cloud.clone_domains.<domain>.r2_bucket
```

Cloud site resolution also requires `LIBRA_D1_API_TOKEN`; Libra reads
`vault.env.LIBRA_D1_API_TOKEN` first, then the exported environment variable, so
the CLI can query the configured D1 database before starting restore.

### `[LOCAL_PATH]`

Optional destination directory. When omitted, Libra infers the directory name from the
repository URL (e.g., `repo` from `repo.git`). If inference fails, an error is returned
asking the user to specify the path explicitly.

```bash
libra clone git@github.com:user/repo.git my-dir
```

### `-b, --branch <NAME>`

Check out `<NAME>` instead of the remote's HEAD. The branch must exist on the remote;
otherwise a "remote branch not found" error is raised.
For `libra+cloud://` sources, use `?ref=<branch|tag|full-ref>` in the URL instead;
`--branch` is rejected before restore starts.

```bash
libra clone -b develop git@github.com:user/repo.git
```

### `--single-branch`

Fetch only the history leading to the tip of a single branch (HEAD, or the branch given
by `-b`). Reduces transfer size for large repositories when only one branch is needed.
Only Git remotes support this transport optimization; `libra+cloud://` restore rejects it
because the restored local repository must preserve all published refs.

```bash
libra clone --single-branch -b main git@github.com:user/repo.git
```

### `--no-single-branch`

Clone the histories of all branches (the default), countermanding an earlier
`--single-branch` (last one on the command line wins). Clone fetches all
branches by default, so on its own this is a no-op.

```bash
libra clone --single-branch --no-single-branch git@github.com:user/repo.git
```

### `--bare`

Create a bare repository without a working tree. The destination directory becomes the
object store directly. Useful for central/server-side repositories.
Bare Cloudflare restores are not part of the first restore surface; `libra+cloud://`
currently rejects `--bare` explicitly.

```bash
libra clone --bare git@github.com:user/repo.git
```

### `--mirror`

Set up a mirror of the source repository (like `git clone --mirror`). Implies
`--bare`, and maps the fetched branches verbatim into `refs/heads/*` and keeps
tags in `refs/tags/*` — without any `refs/remotes/*` tracking refs — then records
the `remote.<name>.mirror=true` marker. Useful for serving or backing up a
repository. Not supported for `libra+cloud://` sources (rejected with
`LBR-CLI-002`).

Narrowings vs Git: (1) Git mirrors `refs/*:refs/*` verbatim; Libra mirrors only
what its fetch transfers — every fetched branch is promoted to `refs/heads/*` and
tags are kept, but ref namespaces Libra does not fetch (e.g. `refs/notes/*`) are
not mirrored. (2) Because Libra's fetch collapses `refs/heads/mr/*` and
`refs/mr/*` into one tracking namespace, any such refs are mirrored as
`refs/heads/mr/*` (provenance is not preserved). (3) The `mirror=true` marker is
informational — no `+refs/*:refs/*` refspec is recorded and `libra fetch` is not
yet mirror-aware, so refreshing the mirror is not automatic.

```bash
libra clone --mirror git@github.com:user/repo.git repo-mirror.git
```

### `--filter <spec>` / `--shallow-since <date>` / `--shallow-exclude <rev>`

Git's fetch-shaping flags that *reduce* what is transferred: `--filter` (e.g.
`blob:none`) is a partial clone, and `--shallow-since`/`--shallow-exclude` bound
shallow history by date or excluded ref. **Libra has no partial-clone/promisor
support, and its fetch supports only `--depth` for shallow history**, so these
flags are accepted but **ignored, with a warning** — the optimization is simply
not applied (the clone still fetches everything those flags would have trimmed,
subject only to `--depth` if also given). Without `--depth` that means a complete
clone — a correct superset of a filtered or date-bounded clone, so the result is
always usable; this mirrors Git itself, which warns and falls back to a full clone
when a server cannot honor `--filter`. `--shallow-exclude` may be given
multiple times. Not supported for `libra+cloud://` sources (rejected with
`LBR-CLI-002`, like `--depth`).

```bash
libra clone --filter blob:none git@github.com:user/repo.git
libra clone --shallow-since "2 weeks ago" git@github.com:user/repo.git
```

### `-l, --local` / `--no-local`

Accepted for Git compatibility and effectively no-ops. Git's `-l`/`--local` asks
for local optimizations (copy/hardlink instead of the transport) when the source
is on the local filesystem, and `--no-local` forces the transport to avoid
hardlinks. Libra **never hardlinks** objects — it always copies — and how it
reads a local-path source is determined by the source type, not by these flags:
a local Libra repository is read directly, while a local Git repository is read
in-process (Libra reads its refs and objects directly, with no `git-upload-pack`
dependency). So both flags are accepted with no effect on the result. The two
override each other; the last one given wins.

```bash
libra clone -l /path/to/source /path/to/dest
```

### `--depth <N>`

Create a shallow clone with history truncated to the specified number of commits.
`N` must be a positive integer.
Only Git remotes support shallow transfer. Cloudflare restore rejects `--depth`
because it must download the complete published object set. A local Libra source
also rejects `--depth` with `LBR-REPO-002`: that transport cannot advertise
shallow boundaries yet, so accepting the option would leave a clone with missing
parents.

```bash
libra clone --depth 1 git@github.com:user/repo.git
libra clone --depth 50 git@github.com:user/repo.git
```

### `--reject-shallow`

Fail if the clone would be a shallow repository that you did not request — i.e.
the source repository is shallow — matching `git clone --reject-shallow`
(exit 128). Combining it with `--depth` is allowed only for transports that can
negotiate shallow boundaries. A local Libra source rejects `--depth` before
object transfer, and no initialized target is left behind.

Two narrowings vs Git: (1) Libra's clone of a local-path source re-fetches the
full history rather than inheriting the source's shallow marker, so this check
is most meaningful when cloning a shallow *remote*; (2) because Libra cannot
distinguish a shallow source from `--depth`-induced shallowness, passing
`--depth` suppresses the post-fetch `--reject-shallow` check for remotes that
do support shallow negotiation (Git would still reject a shallow source with
`--depth`).

```bash
libra clone --reject-shallow git@github.com:user/repo.git
```

### `--reference <repo>` / `--reference-if-able <repo>` / `--shared` (`-s`) / `--dissociate`

Git's object-sharing flags, which set up `objects/info/alternates` so a clone
borrows or shares objects with another local store. **Libra has no object
alternates** — it always copies every object into the clone — so a Libra clone is
always fully self-contained. These flags are therefore accepted for
compatibility as **no-ops**:

- `--reference <repo>` and `--shared` (`-s`) emit an explanatory warning that
  they had no effect (objects are copied, not borrowed/shared). `--reference` may
  be given multiple times.
- `--reference-if-able <repo>` is silently ignored — matching Git, which silently
  drops a reference it cannot use (here, none are usable). May be given multiple
  times.
- `--dissociate` is a silent no-op: there is never a borrow to dissociate.

The clone still succeeds and produces a complete, self-contained repository.

```bash
libra clone --reference /path/to/local/mirror git@github.com:user/repo.git
libra clone --dissociate git@github.com:user/repo.git
```

### `--tags` / `--no-tags`

`libra clone` fetches **all** tags by default (matching Git). `--no-tags` clones
without any tags and records `remote.<name>.tagOpt=--no-tags` (the remote name is
`origin` by default, or the `-o`/`--origin` value), so subsequent
`libra fetch` calls also skip tags. `--tags` is accepted for compatibility and to
override an earlier `--no-tags` (last flag wins).

```bash
libra clone --no-tags git@github.com:user/repo.git
```

### `--no-progress`

Suppress the fetch progress meter (the "Receiving objects" spinner) during the
clone, matching `git clone --no-progress`. Other output is unaffected.

```bash
libra clone --no-progress git@github.com:user/repo.git
```

### `--no-checkout`

Do not check out HEAD into the working tree after cloning, matching `git clone
--no-checkout`. Objects, refs and HEAD are still set up — only the working-tree
checkout is skipped, so the destination contains the repository metadata but no
checked-out files.

```bash
libra clone --no-checkout git@github.com:user/repo.git
```

### `-o`, `--origin <NAME>`

Use `<NAME>` for the remote (and its `refs/remotes/<NAME>/*` tracking refs)
instead of the default `origin`, matching `git clone -o`. The branch tracking
config (`branch.<branch>.remote`) and `remote.<NAME>.url` use the chosen name.
This applies to standard clones; `libra+cloud` clones always use `origin`.

```bash
libra clone -o upstream git@github.com:user/repo.git
```

### `--deps-of <path>` / `--deps-depth-limit <N>` (dependency-filtered clone, lore.md 3.2)

Libra-only extension (`intentionally-different` — Git has no file-dependency
concept). After a normal, **fully checked-out and commit-safe** clone, scope the
read-only sparse VIEW ([`sparse-view`](sparse-view.md), lore.md 2.2) to the
forward dependency closure ([`deps`](deps.md), lore.md 3.1) of the given root
path(s). `--deps-of` is repeatable; `--deps-depth-limit <N>` bounds the closure
depth (`1` = direct dependencies only). It implies `--notes` (the dependency
graph must be fetched to compute the closure) and records
`remote.<name>.fetchNotesDeps=true` so later `libra pull` keeps the graph fresh.

This is **not** partial clone (`--filter`) and **not** `--sparse` (declined,
D10): objects are never wire-filtered — the whole pack is downloaded and the
whole tree stays on disk. Only the VIEW is narrowed (`ls-files`/`status`/`diff`
scope to the closure); reducing on-disk footprint is deferred (D18, needs the
D10 skip-worktree machinery). Only a **local Libra source** can travel the
dependency graph in v1 (D17); a network or plain-Git source performs a full
clone without scoping and warns. Conflicts with `--no-checkout`/`--bare`/
`--mirror` (they skip the checkout that keeps the repository commit-safe) and is
rejected for `libra+cloud://` sources.

```bash
libra clone --deps-of scene.usd /path/to/local-libra-repo my-scene
libra clone --deps-of a.txt --deps-depth-limit 1 /path/to/src direct-only
```

## Common Commands

```bash
libra clone git@github.com:user/repo.git
libra clone https://github.com/user/repo.git
libra clone git@github.com:user/repo.git my-dir
libra clone --bare git@github.com:user/repo.git
libra clone --no-checkout git@github.com:user/repo.git
libra clone -b develop git@github.com:user/repo.git
libra clone --single-branch -b main git@github.com:user/repo.git
libra clone --depth 1 git@github.com:user/repo.git
```

## Human Output

Default human mode writes staged progress to `stderr` and the final summary to `stdout`.

Phases:

- `Connecting to <url> ...`
- `Initializing repository ...`
- `Fetching objects ...`
- `Configuring repository ...`
- `Checking out working copy ...` (non-bare only)

Success output:

```text
Cloned into 'repo'
  remote: origin -> git@github.com:user/repo.git
  branch: main
  signing: enabled

Tip: using existing SSH key at ~/.ssh/id_ed25519
```

Bare clone:

```text
Cloned into bare repository '/path/to/repo.git'
  remote: origin -> git@github.com:user/repo.git
  branch: main
  signing: enabled
```

Empty remote:

```text
Cloned into 'empty'
  remote: origin -> git@github.com:user/empty.git
  signing: enabled

warning: You appear to have cloned an empty repository.
```

`--quiet` suppresses all progress and the final success summary, including warnings.

## Structured Output

`libra clone` supports the global `--json` and `--machine` flags.

- `--json` writes one success envelope to `stdout`
- `--machine` writes the same schema as compact single-line JSON
- both suppress progress output and nested init/fetch output
- `stderr` stays clean on success

Example:

```json
{
  "ok": true,
  "command": "clone",
  "data": {
    "path": "/Users/eli/projects/my-repo",
    "bare": false,
    "remote_url": "git@github.com:user/repo.git",
    "remote_name": "origin",
    "branch": "main",
    "object_format": "sha1",
    "repo_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "vault_signing": true,
    "ssh_key_detected": "/Users/eli/.ssh/id_ed25519",
    "shallow": false,
    "warnings": [],
    "gitignore_converted": [".libraignore"],
    "objects_fetched": 42,
    "bytes_received": 4096
  }
}
```

Empty remote returns `"branch": null` and a warning:

```json
{
  "ok": true,
  "command": "clone",
  "data": {
    "path": "/Users/eli/projects/empty-repo",
    "bare": false,
    "remote_url": "git@github.com:user/empty-repo.git",
    "remote_name": "origin",
    "branch": null,
    "object_format": "sha1",
    "repo_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "vault_signing": true,
    "ssh_key_detected": null,
    "shallow": false,
    "warnings": [
      "You appear to have cloned an empty repository."
    ],
    "gitignore_converted": [],
    "objects_fetched": 0,
    "bytes_received": 0
  }
}
```

### Schema Notes

- `remote_name` is the configured remote's name (`origin` by default, or the `-o`/`--origin` value for standard clones)
- `branch` is the actual checked-out branch; `null` when the remote has no refs
- `shallow` is `true` when `--depth` was used
- `gitignore_converted` lists the worktree-relative `.libraignore` files written from converted `.gitignore` files; always present (empty for bare clones or when the source has no `.gitignore`)
- `source_kind` and `cloud_site` are omitted for ordinary Git/local clones; `libra+cloud://` clones add them with clone domain, site id, slug, repo id, selected ref, and restored revision
- `ref_format` and `converted_from` from init are intentionally excluded
- `objects_fetched` / `bytes_received` report the fetch pack's object count and byte size for Git sources; they are omitted for `libra+cloud://` restores (which download indexed objects from R2 rather than a pack stream)

## Design Rationale

### No `--recurse-submodules`

Git's submodule system (`--recurse-submodules`) is a frequent source of developer friction:
submodules require separate fetch/checkout cycles, create nested `.git` directories, and
break many tools that assume a single worktree. Libra does not implement submodules. For
monorepo workflows, all code lives in a single repository. For multi-repo composition, Libra
encourages explicit dependency management (package managers, vendoring) rather than embedding
repositories within repositories. This keeps the clone operation simple and predictable.

### Vault bootstrapping during clone

Libra initializes vault-backed signing during clone by reusing the same `run_init()` path
as `libra init`. This means every cloned repository is immediately ready for signed commits
without additional setup. Git requires users to manually configure GPG/SSH signing after
cloning, which means most cloned repositories produce unsigned commits by default. By
bootstrapping the vault at clone time, Libra ensures that the security posture of a cloned
repository matches that of a freshly initialized one.

### Ignore file conversion

Libra uses `.libraignore` for its ignore policy. During non-bare clone, every checked-out
`.gitignore` is copied to a sibling `.libraignore`. Existing user-owned `.libraignore` files
are preserved and surfaced as warnings; the original `.gitignore` files remain untouched.

### `--depth` for shallow clones

Shallow clones are essential for CI/CD pipelines and large monorepos where full history is
unnecessary. Libra supports `--depth N` for Git remotes that negotiate shallow
boundaries: the history is truncated to the specified number of commits. The
depth value is validated at parse time (must be a positive integer) and
propagated to the fetch protocol layer. Local Libra sources fail closed with
`LBR-REPO-002` until they can produce shallow boundary metadata. Libra bounds
shallow history **only** by `--depth`: the date/ref-based `--shallow-since` and
`--shallow-exclude` flags are accepted but ignored with a warning (see their Options entry
above) rather than rejected, so scripts that pass them still clone successfully.

### `--sparse` is intentionally unsupported

Sparse-checkout (`git clone --sparse`, `git sparse-checkout`) is intentionally not
implemented. Sparse cone/skip-worktree relies on Git-managed worktree configuration,
while Libra has migrated config / HEAD / refs to SQLite. The bridge is not free, and
the audit-driven decision is to keep `--sparse` deferred until there is a concrete
monorepo subtree-checkout requirement that cannot be met by tiered cloud storage.
See [`docs/development/commands/_compatibility.md`](../development/commands/_compatibility.md)
entry **D10** for the restart conditions.

### `--recurse-submodules` is intentionally unsupported

Per the broader product boundary on submodules (no submodule subcommand surface),
`clone --recurse-submodules` is also unsupported. See
[`docs/development/commands/_compatibility.md`](../development/commands/_compatibility.md)
entries **D1** (submodule) and **D4** (clone --recurse-submodules) for restart
conditions.

### `--single-branch` flag

When combined with `--branch`, `--single-branch` reduces the data transferred during clone
by fetching only the specified branch's history. This is particularly useful for large
repositories with many long-lived branches where only one branch is needed for the current
workflow (e.g., CI building a specific release branch). Git supports this as well; jj does
not, because its operation-log model fetches all refs by design.

## Parameter Comparison: Libra vs Git vs jj

| Parameter / Flag | Git | jj | Libra |
|---|---|---|---|
| Remote URL (positional) | `git clone <url>` | `jj git clone <url>` | `libra clone <url>` |
| Destination directory | `git clone <url> <dir>` | `jj git clone <url> <dir>` | `libra clone <url> <dir>` |
| Specific branch | `-b` / `--branch` | `-b` / `--branch` (jj 0.17+) | `-b` / `--branch` |
| Single branch | `--single-branch` | N/A | `--single-branch` |
| No single branch | `--no-single-branch` | N/A | `--no-single-branch` (countermands `--single-branch`; all branches is the default) |
| Bare clone | `--bare` | N/A | `--bare` |
| Shallow clone (depth) | `--depth <n>` | N/A | supported for Git remotes; local Libra sources fail closed (`LBR-REPO-002`); cloud rejects |
| Shallow since date | `--shallow-since=<date>` | N/A | accepted no-op for Git remotes (ignored + warning; not applied, history bounded only by `--depth`); rejected for cloud |
| Shallow exclude | `--shallow-exclude=<rev>` | N/A | accepted no-op for Git remotes (ignored + warning; not applied, history bounded only by `--depth`); rejected for cloud |
| Mirror clone | `--mirror` | N/A | `--mirror` (implies `--bare`; mirrors fetched branches into `refs/heads/*`, keeps tags, no tracking refs, sets `remote.<name>.mirror` marker; narrowed — only fetched branches/tags, refresh not mirror-aware) |
| Reference repository | `--reference <repo>` / `--reference-if-able <repo>` | N/A | accepted no-op (Libra always copies objects, no alternates); `--reference` warns, `--reference-if-able` silent |
| Shared object store | `--shared` / `-s` | N/A | accepted no-op (always copies); warns |
| Dissociate from reference | `--dissociate` | N/A | accepted no-op (already self-contained); silent |
| No hardlinks | `--no-hardlinks` | N/A | N/A |
| Recurse submodules | `--recurse-submodules` | N/A | N/A (no submodules) |
| Shallow submodules | `--shallow-submodules` | N/A | N/A |
| Separate git dir | `--separate-git-dir=<dir>` | N/A | N/A (removed) |
| Template directory | `--template=<dir>` | N/A | N/A (handled by init internally) |
| Quiet mode | `-q` / `--quiet` | `--quiet` | `--quiet` (global flag) |
| Verbose / progress | `--progress` / `--verbose` | N/A | Phased stderr progress (default) |
| No checkout | `-n` / `--no-checkout` | N/A | `--no-checkout` |
| Sparse checkout | `--sparse` | N/A | N/A |
| Filter (partial clone) | `--filter=<spec>` | N/A | accepted no-op for Git remotes (ignored + warning; not applied, history bounded only by `--depth`); rejected for cloud |
| Bundle URI | `--bundle-uri=<uri>` | N/A | N/A |
| Vault signing bootstrap | N/A | N/A | Always enabled (matches init) |
| SSH key detection | N/A | N/A | Automatic detection + hint |
| Structured JSON output | N/A | N/A | `--json` / `--machine` |
| Error hints | Minimal messages | Minimal messages | Every error type has an actionable hint |

## Error Handling

Every `CloneError` variant maps to an explicit `StableErrorCode` -- no message substring inference.

| Scenario | Error Code | Exit | Hint |
|----------|-----------|------|------|
| Cannot infer destination path | `LBR-CLI-002` | 129 | "please specify the destination path explicitly" |
| Destination exists and is non-empty | `LBR-CLI-003` | 129 | "choose a different path or empty the directory first" |
| Destination already contains a repo | `LBR-REPO-003` | 128 | "the destination already contains a libra repository" |
| Cannot create destination directory | `LBR-IO-002` | 128 | "check directory permissions and disk space" |
| Local path does not exist | `LBR-REPO-001` | 128 | "use a valid libra repository path or a reachable remote URL" |
| Malformed URL or unsupported scheme | `LBR-CLI-003` | 129 | "check the clone URL or scheme" |
| Authentication / permission denied | `LBR-AUTH-002` | 128 | "check SSH key / HTTP credentials and repository access rights" |
| Network unreachable | `LBR-NET-001` | 128 | "check the remote host, DNS, VPN/proxy, and network connectivity" |
| Protocol / discovery error | `LBR-NET-002` | 128 | "the remote did not complete discovery successfully" |
| Remote branch not found | `LBR-REPO-003` | 128 | "use `-b <branch>` to specify an existing branch" |
| Object format mismatch | `LBR-REPO-003` | 128 | "the remote and local repository use different object formats" |
| Checkout resolve failure | `LBR-REPO-003` | 128 | "working tree checkout target could not be resolved" |
| Checkout read failure | `LBR-IO-001` | 128 | "failed to read repository state while checking out" |
| Checkout write failure | `LBR-IO-002` | 128 | "files could not be written" |
| Checkout LFS download failure | `LBR-NET-001` | 128 | "LFS content transfer failed" |
| Internal invariant | `LBR-INTERNAL-001` | 128 | Issues URL |

Init errors are transparently forwarded through `InitError -> CliError`.

### Cleanup Failure Visibility

When clone fails, `cleanup_failed_clone()` attempts to remove the partially created directory.
If cleanup itself fails, the warning is attached to the error via `with_priority_hint()` so it
surfaces in both human and JSON error output instead of being silently swallowed.

### Non-Bare Checkout Is Required For Success

`setup_repository()` uses `execute_checked_typed()` which returns typed `RestoreError` variants.
If checkout fails, the clone reports failure -- it does not silently succeed with a broken worktree.

## Vault And Identity

- Clone always initializes with `vault: true`, matching `libra init` defaults
- `vault_signing` and `ssh_key_detected` from init are transparently forwarded to `CloneOutput`
- SSH key detection uses the isolated `HOME` from the init phase

## Compatibility Notes

- `--recurse-submodules` is not supported; Libra does not implement submodules
- `--reference`/`--reference-if-able`/`--shared`/`--dissociate` are accepted no-ops (Libra has no object alternates — it always copies objects — so a clone is already self-contained; `--reference`/`--shared` warn, the others are silent)
- Clone always bootstraps vault signing; use `libra config` to disable after cloning if needed
- The `--depth` value must be a positive integer; zero or negative values are rejected at parse time
- `--no-checkout` sets up objects/refs/HEAD but skips the working-tree checkout; use `--bare` instead when you want no working tree at all (no `.libra` worktree layout)
