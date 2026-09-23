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

Configuration schema compatibility is role-scoped. Before `libra clone` trusts
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

Checked-out entries carry the tree mode's permission bits (`100755` executable, `100644` plain) under the process `umask` (plan issues/470 FM-01).

## Options

### `<REMOTE_REPO>` (required)

The remote repository URL to clone from. Supports SSH (`git@host:user/repo.git`) and
HTTPS (`https://host/user/repo.git`) protocols, as well as local filesystem paths.
The former Cloudflare publish restore source was removed with the Publish product;
cloning that source is a usage error (exit 129) and points at a git remote or
`libra cloud` for repository backup. The matching clone-domain config keys are
frozen and are no longer read.

```bash
libra clone git@github.com:user/repo.git
libra clone https://github.com/user/repo.git
libra clone /path/to/local/repo
```

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

```bash
libra clone -b develop git@github.com:user/repo.git
```

### `--single-branch`

Fetch only the history leading to the tip of a single branch (HEAD, or the branch given
by `-b`). Reduces transfer size for large repositories when only one branch is needed.
`--depth`, `--shallow-since`, and `--shallow-exclude` imply this flag unless
`--no-single-branch` is given (matching `git clone`). A single-branch clone writes
`remote.<name>.fetch=+refs/heads/<branch>:refs/remotes/<name>/<branch>`. Only Git remotes
support this transport optimization.

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

```bash
libra clone --bare git@github.com:user/repo.git
```

### `--mirror`

Set up a mirror of the source repository (like `git clone --mirror`). Implies
`--bare`, and maps the fetched branches verbatim into `refs/heads/*` and keeps
tags in `refs/tags/*` — without any `refs/remotes/*` tracking refs — then records
the `remote.<name>.mirror=true` marker. Useful for serving or backing up a
repository.

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
multiple times.

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
`N` must be a positive integer. Implies `--single-branch` unless `--no-single-branch`
is given (matching `git clone`).
Only Git remotes support shallow transfer. A local Libra source
rejects `--depth` with `LBR-REPO-002`: that transport cannot advertise
shallow boundaries, so accepting the option would leave a clone with missing
parents. This fail-closed behavior is the accepted end state (decision D20 in
the development compatibility register), not a pending gap.
A local Git source reached with `file://` or `--no-local` truncates by the
shortest distance from any wanted tip, then one boundary pass: a commit is
shallow when a parent was not sent, or when a root commit sits exactly on the
depth cutoff (issues/474 CL-04).

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
This applies to standard clones.

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
`--mirror` (they skip the checkout that keeps the repository commit-safe).

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
- `source_kind` and `cloud_site` are omitted for ordinary Git/local clones
- `ref_format` and `converted_from` from init are intentionally excluded
- `objects_fetched` / `bytes_received` report the fetch pack's object count and byte size for Git sources

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
`LBR-REPO-002` — the accepted end state (decision D20), since they cannot produce shallow boundary metadata. Libra bounds
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
by fetching only the specified branch's history. `--depth` / `--shallow-since` /
`--shallow-exclude` imply the same narrowing unless `--no-single-branch` is given.
This is particularly useful for large
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
| pkt-line discovery / transfer framing error | `LBR-NET-002` | 128 | "check that the remote serves Git data and that a proxy has not altered the response" |
| Other discovery protocol error | `LBR-NET-002` | 128 | "the remote did not complete discovery successfully; retry and inspect server/protocol settings" |
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

The same protocol classification and hint apply during object transfer, including
a truncated pkt-line header or payload. An ordinary IO failure without a pkt-line marker during discovery
remains `LBR-IO-001`; host-key diagnostic handling is described in the SSH section below.

Pack completeness is separate from pkt-line framing: an incomplete pack ending
at a clean frame boundary keeps clone's existing `LBR-NET-001` transfer error and
network retry hint. Fetch and pull report that completeness failure as `LBR-NET-002`.

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
