# `libra init`

Create an empty Libra repository or reinitialize an existing one.

## Synopsis

```
libra init [OPTIONS] [DIRECTORY]
```

## Description

`libra init` creates a new Libra repository, seeds the SQLite-backed metadata in
`.libra/libra.db`, configures `HEAD`, and optionally imports an existing local Git repository.

Running `libra init` in an existing directory creates a `.libra` subdirectory with the
object store, SQLite database, default configuration, HEAD pointing to the initial branch,
and (by default) a vault-backed PGP signing key. If `DIRECTORY` is given and does not exist,
it is created first.

Like Git, `libra init` leaves the fresh worktree clean: it does **not** create a default
`.libraignore`. Create one yourself (or convert an existing `.gitignore`) when you need
Libra-specific ignore rules; ignore sources are read lazily and need no initialization.

When `--from-git-repository` is supplied, objects and refs are imported from the source Git
repository and `origin` is configured to point at the source branch layout. The converted
repository reports the source repository's actual `HEAD` branch as `initial_branch`; the
configured `init.defaultBranch` is not used to rename or misreport that imported branch. Any
`.gitignore` files found in the source worktree or checked-out import are copied to matching
`.libraignore` files.

Running `libra init` again inside an already-initialized repository is safe: like
`git init`, it re-initializes in place, printing `Reinitialized existing Libra
repository in <path>` and re-creating any missing standard layout (templates,
directories) and re-applying `--shared`, while preserving existing repository data —
configuration, `HEAD`, refs, objects, vault, and repository id are otherwise untouched.
Opening the database can still apply schema migrations; preserving data does not
mean preserving the old database schema.
`--initial-branch` that differs from the existing HEAD is ignored (with a warning).
A different `--object-format` is refused with `LBR-CLI-002` before any layout or
database side effect. `--from-git-repository` is rejected on an already-initialized
repository.

Initializing a repository whose storage root is the Libra home itself (`$LIBRA_HOME`,
default `~/.libra`), or a path inside the global configuration directory
(`<XDG_CONFIG_HOME or ~/.config>/libra`), is refused. These directories store
per-user state — the global config database, vault keys, and binaries — and must never mix
in repository data, which lives in a project's own `.libra/libra.db` (or a dedicated
bare repository directory).
If `LIBRA_HOME` and the global config location differ, both directories are reserved.
This also applies to linked worktree `commondir` targets and to paths whose `.libra`
directory does not exist yet. Repository discovery never treats these directories
as a repository, even if a stray
`libra.db` exists there, so commands run from the home directory resolve configuration
through the global scope instead of reading the home database as repo-local config.

Opening an older repository or global config database automatically recreates a missing
legacy `config` table without changing existing configuration. The ignored home
`libra.db` artifact is left untouched; commands no longer open it as a repository.

### Operation-v2 convergence

The `2026090801` convergence migration is part of released builds. It preserves
the migration receipts and configuration values described below. Migration
`2026091801` subsequently retires the temporary legacy operation namespace.

- The forward-only `2026090801` migration must retain the original
  `2026090101` (`operation_v2`) and `2026090601` (`legacy_config_table`, #472)
  migration receipts, including existing receipt timestamps. A #472 repository
  with v1 operations needs the missing operation-v2 migration, not just a newer
  maximum version number.
- The earlier convergence migration temporarily isolated v1 operation data in
  the `legacy_operation*` namespace. After the v2 runtime cutover, migration
  `2026091801` retires that namespace forward-only; historical migration tests
  retain the old schema only as upgrade fixtures. Existing `config` and
  `config_kv` values and the #472 missing-table repair remain preserved.
- Once `2026090801` commits, a binary supporting schemas only through
  `2026090601` must refuse the newer repository schema before using incompatible
  operation tables. The package version string alone does not identify a
  binary's supported schema.
- Before upgrading a live database, retain a verified, consistent pre-upgrade
  backup, using SQLite's online-backup mechanism rather than copying only a
  live DB file without its WAL. Keep the backup protected and retain the old
  binary. Linked worktrees share the repository database, so another worktree
  is not an isolated upgrade test.
- This migration has no down path. Running an old binary, re-running `init`, or
  deleting migration receipts is not a downgrade procedure. Binary-install
  rollback does not restore the database; recovery needs the matching
  pre-upgrade backup and binary.

See [operation capture limits](op.md#current-branch-capture-contract)
and the [convergence rationale](../development/commands/op.md#current-branch-convergence-contract).

## Options

### `[DIRECTORY]`

Positional argument specifying the directory to initialize. Defaults to `.` (the current
working directory) when omitted.

```bash
libra init my-project          # creates ./my-project/.libra
libra init                     # creates ./.libra
```

### `--bare`

Create a bare repository. Bare repositories have no working tree and are used as central
remote targets. The repository directory itself becomes the object store.

```bash
libra init --bare my-repo.git
```

### `-b, --initial-branch <NAME>`

Override the name of the initial branch. When the flag is omitted for a new repository, Libra
reads `init.defaultBranch` from local, global, then system config (Git-style variable-name
case-insensitive lookup) and falls back to `main` if that key is unset. Local and global
encrypted values are decrypted before validation. An unreadable local or global config fails
with `LBR-IO-001` — except a future-schema global store (newer than this binary), which is
skipped with a one-time warning (see `LBR-CONFIG-001`); an unreadable or unsupported system
config scope is skipped. For
`--from-git-repository`, the source repository's `HEAD` branch wins so the import cannot claim
a configured default that differs from the converted refs.
The branch name is validated against the same rules as `git check-ref-format`: no spaces,
no `..`, no ASCII control characters, maximum 255 characters.

```bash
libra init -b develop
libra init --initial-branch trunk
```

### `--object-format <FORMAT>`

Set the object hash algorithm for a **fresh** repository. Accepted values are
`sha1` (default), `sha256`, and `blake3` (Libra extension; not a Git object format).

```bash
libra init --object-format sha256
libra init --object-format blake3
```

Convert (`--from-git-repository`) is SHA-1 Git → SHA-1 Libra only. Passing
`--object-format sha256` or `blake3` with Convert, or converting a SHA-256 Git
source, fails closed with `LBR-CLI-002` before any target write. Re-running
`init` on an existing repository refuses a different `--object-format` the same
way; create a new repository instead.

### `--from-git-repository <PATH>`

Import objects and refs from an existing local Git repository. The source must contain
valid `HEAD`, `config`, and `objects` structures. An `origin` remote is configured pointing
to the imported branch layout, and the JSON/human result reports the source `HEAD` branch.
`init.defaultBranch` is not consulted for this conversion path. Empty Git repositories
(no refs) produce an error.

For non-bare imports, Libra converts every `.gitignore` it can see into a sibling
`.libraignore`. Existing user-owned `.libraignore` files are preserved and reported as
warnings in structured output.

When converting into an existing bare target directory, Libra refuses before writing
if a `libra.db` database or SQLite sidecar already exists, or if `--vault` is enabled
and a `vault.db` database or sidecar already exists. Move the conflicting file aside
or choose another target; unrelated files in the directory are preserved.

```bash
libra init --from-git-repository ../old-project
```

**Interruption and recovery.** The conversion is protected by a marker in the storage
root (`CONVERSION_IN_PROGRESS`), installed after the initial layout and removed only
after everything succeeded:

- after the marker is installed, an ordinary conversion error rolls back the
  metadata created by this run; on Unix, SIGINT/SIGTERM also triggers rollback.
  The conversion can then be retried immediately. An existing bare target
  directory and its unrelated user files remain in place; the same protection
  applies when the target path contains symlinks or `..`. User files and the
  source `.git` are never touched. On Windows, Libra closes the new repository
  database pool before deleting its SQLite files. If closing fails or times
  out after 10 seconds, Libra keeps the marker and reports manual recovery
  steps instead of attempting a partial rollback. Windows has no signal
  rollback handler; an interrupted process may leave the marker and requires
  the recovery steps below;
- a hard kill (SIGKILL, OOM, power loss) leaves the marker behind. Until it is cleared,
  other commands refuse instead of presenting the repository as empty: while the
  recorded owner is still running they return `LBR-CONFLICT-002`, and after the
  hard kill they return `LBR-REPO-003` ("incomplete Git conversion");
- retrying `libra init --from-git-repository <same-source>` after a hard kill verifies that
  the recorded owner process is gone and that the source still matches, renames the
  incomplete storage root to a sibling `<storage>.incomplete-<timestamp>` (**never
  deletes it**), and converts from scratch. For an existing bare target, that aside
  contains its pre-existing user files; inspect it before removing anything. The aside
  path is reported in the warnings;
- on macOS, conversions started by a v0.30.85 build record enough owner
  identity to recognize an exited process, so the same-source retry can
  recover after a hard kill in the same boot session.
  The earlier published `v0.31.0` binary predates this fix; see the
  [signed stable installer guidance](upgrade.md) if it is installed. The marker keeps the legacy owner
  field empty, so older macOS binaries also refuse a concurrent retry. Markers
  from an older binary, another host, or a prior boot cannot prove the owner
  has exited; confirm no conversion is running on any host, then follow the
  `LBR-REPO-003` manual-recovery guidance instead of deleting the storage root;
- a retry is refused when the conversion owner is still running (`LBR-CONFLICT-002`), and
  when the recorded source differs from the one being converted (`LBR-REPO-003`).

### `--vault <BOOL>`

Enable or disable vault-backed PGP signing. Defaults to `true`. When enabled, Libra
generates a PGP signing key during initialization and stores it in the vault. Set to
`false` to skip vault setup entirely.

```bash
libra init --vault false
```

### `--template <PATH>`

Path to a template directory whose contents are copied into the new `.libra` directory.
An empty value (`--template=`) means "no template", like Git: nothing is copied and the
built-in `info/exclude` + hook templates are skipped.

The template can never be copied into itself. A template that **is** the target
repository is refused with a CLI error (`LBR-CLI-002`), and a template that **contains**
the target (for example `--template=.` while creating `./repo`) skips the target
worktree, so repeated `libra init --template=. repo` calls never nest
`repo/.libra/repo/.libra/…`. This also holds when the target path uses a symlink
or `..`. A template inside the target's `.libra` storage is refused before copying.
The resolved target is also used for the final repository check: a `missing/..`
alias can initialize its existing parent. On Unix, a path through an existing
non-directory component is rejected with `LBR-REPO-002` (exit 128) before any
repository files are written.

```bash
libra init --template /path/to/template
libra init --template=            # no template at all
```

### `--shared[=<MODE>]`

Specify that the repository is to be shared amongst several users (mirrors the Git
`--shared` flag for group permissions). Passing `--shared` without a value defaults to
`group`. When specifying a mode, use `--shared=<MODE>`. Supported values are `false`,
`umask`, `no`, `off`, `0`, `true`, `group`, `yes`, `on`, `1`, `all`, `world`,
`everybody`, `2`, or an octal file mode such as `0660`, `0640`, `0600`, or `0664`.

`group`/`true`/`1` is recorded in `core.sharedRepository` as `1`; `all`/`world`/`everybody`/`2`
is recorded as `2`; octal modes are masked with `0666` and persisted in `0%o` format (e.g. `0660`).
`umask`/`false`/`0` does not persist `core.sharedRepository` and removes any existing value on
re-initialization. Legacy Libra values (`group`, `all`, `umask`, `false`, `0775`) remain fully
readable and are rewritten to the Git format upon re-initialization. When `--shared` is omitted,
the cascade (`local` -> `global` -> `system`) for `core.sharedRepository` is honored.

Numeric modes are validated before any repository layout is written. The file owner must always
retain read and write permissions (`mode & 0600 == 0600`). Modes missing owner read/write access
such as `0400` or `0200` are rejected with `LBR-CLI-002` before any files are created.

Permissions for files and directories are computed separately aligning with Git's `calc_shared_perm`:
- Regular files receive shared bits overlay (`0660` for `group`, `0664` for `all`/`everybody`; numeric modes replace the lower 9 bits). Executable files copy read bits to execute bits (`mode |= (mode & 0444) >> 2`).
- Directories copy read bits to execute bits for directory traversal and receive the setgid bit (`02000`) whenever group access is present (`group`, `all`, or numeric mode with group read/write).
- Private signing keys in `vault.db` and its SQLite sidecars are strictly excluded and remain owner-only.
- Symlinks within the repository layout are never followed during permission sweeps.

For a Git conversion into an existing bare target, `--shared` updates the target
directory and newly created repository entries; pre-existing user files retain
their permissions.

```bash
libra init --shared shared-repo
libra init --shared=group shared-repo
libra init --shared=0660 shared-repo
```

### `--ref-format <FORMAT>`

Select the branch-name validation rules. Accepted values: `strict` (default) and
`filesystem`. Libra stores refs in SQLite, so Git's storage backends `files` and
`reftable` are rejected with a hint (intentional difference, D25).

### `-q, --quiet`

Suppress progress and success output. Only errors are printed.

```bash
libra init -q my-project
```

## Common Commands

```bash
libra init
libra init my-project
libra init --bare my-repo.git
libra init -b develop
libra init --object-format sha256
libra init --from-git-repository ../old-project
libra init --vault false
libra init --shared group shared-repo
```

## Human Output

Default human mode writes staged progress to `stderr` and the final confirmation to `stdout`.

Phases include:

- `Creating repository layout ...`
- `Initializing database ...`
- `Setting up refs ...`
- `Converting from Git repository at ...` when `--from-git-repository` is used
- `Generating PGP signing key ...` when vault signing is enabled

Success output uses past tense:

```text
Initialized empty Libra repository in /path/to/repo/.libra
  branch: main
  signing: enabled
```

`--quiet` suppresses both progress and the final success summary.

## Structured Output

`libra init` supports the global `--json` and `--machine` flags.

- `--json` writes one success envelope to `stdout`
- `--machine` writes the same schema as compact single-line JSON
- both suppress progress output
- `stderr` stays clean on success, including `--from-git-repository`

Example:

```json
{
  "ok": true,
  "command": "init",
  "data": {
    "path": "/path/to/repo/.libra",
    "bare": false,
    "initial_branch": "main",
    "object_format": "sha1",
    "ref_format": "strict",
    "repo_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "vault_signing": true,
    "converted_from": null,
    "ssh_key_detected": "/Users/alice/.ssh/id_ed25519",
    "warnings": [],
    "reinitialized": false
  }
}
```

## Design Rationale

### SQLite instead of flat files for metadata

Git stores configuration in flat `.git/config` (INI format), refs as individual files under
`.git/refs/`, and reflogs as append-only text files. This approach suffers from race conditions
on concurrent writes, requires directory-level locking (`*.lock` files), and makes atomic
multi-ref updates impossible without the `packed-refs` mechanism.

Libra stores all metadata (config, refs, reflogs, rebase state) in a single SQLite database
at `.libra/libra.db`. SQLite provides ACID transactions, concurrent-reader/single-writer
semantics via WAL mode, and efficient queries without scanning the filesystem. This design
eliminates an entire class of corruption bugs that plague Git on networked filesystems (NFS,
CIFS) and makes operations like "find all branches matching a pattern" O(log n) instead of
a directory walk.

### Vault signing enabled by default

Modern development workflows increasingly require commit provenance (signed commits for
supply-chain security, verified merges in CI). Git leaves signing as a manual opt-in
requiring external GPG/SSH key management. Libra takes the opposite stance: vault-backed
PGP signing is enabled at `init` time, generating a key automatically. Developers who do
not need signing can opt out with `--vault false`, but the secure-by-default path means
new repositories are immediately ready for verified workflows without additional setup.

### No `--separate-git-dir` / `--separate-libra-dir`

Git supports decoupling the `.git` directory from the worktree via `--separate-git-dir`,
creating a `gitdir:` pointer file. This feature is rarely used, adds complexity to every
path-resolution routine, and creates subtle breakage when the pointer file or target
directory is moved independently. Libra removed this feature in favor of always co-locating
`.libra/` with the worktree root, simplifying the repository discovery algorithm and
eliminating a source of user confusion.

Both flags are still *parsed*: passing them fails with `LBR-CLI-002` (exit 129) and a
migration hint instead of a generic "unexpected argument" error (intentional difference
D25). **Migrating a Git repository that uses `--separate-git-dir`** only needs:

```bash
libra init --from-git-repository <worktree> <new-dir>
# or point directly at the pointer file:
libra init --from-git-repository <worktree>/.git <new-dir>
```

`--from-git-repository` resolves the `gitdir:` pointer (absolute, or relative to the
pointer file) before converting, copying objects and refs into the new Libra repository.

### `--recurse-submodules`

Not implemented (D25): the flag is parsed and refused with `LBR-CLI-002` and a hint to
run `libra submodule init` after creating the repository, instead of a parse error.

### `--from-git-repository` instead of Git's lack of import

Git has no built-in concept of importing from another VCS format into itself at init time;
the closest equivalent is `git clone --local`. jj provides `jj git init --git-repo` for
co-located operation with a Git backend. Libra's `--from-git-repository` provides a one-time,
one-directional import that copies objects and refs from a local Git repository into a new
standalone Libra repository. This is a deliberate design choice: rather than wrapping Git
(as jj does), Libra creates a fully independent `.libra` store, making it a standalone VCS
rather than a Git frontend.

### Default branch follows `init.defaultBranch`

Following the industry-wide convention shift, Libra falls back to `main` as the initial
branch name. `init.defaultBranch` supplies the default when configured, and `-b` /
`--initial-branch` overrides all config scopes for one new-repository invocation. Local/global
encrypted values are decrypted before validation. An empty or invalid configured branch fails
with `LBR-CLI-002` before repository layout is written; unreadable local/global config fails
with `LBR-IO-001`, while an unreadable or unsupported system scope is skipped. Exception: a
global config store whose schema is newer than this Libra binary is skipped with a one-time
deduplicated warning instead of failing (see `LBR-CONFIG-001`). A Git conversion
reports the source `HEAD` branch instead of using this default.

### jj comparison

jj (`jj git init`) wraps a Git backend and does not create its own object store; it stores
jj-specific metadata (operation log, view) alongside the `.git` directory. Libra creates a
fully independent `.libra` store with its own object format, making it a standalone VCS
rather than a Git frontend. The `--from-git-repository` flag provides a one-time import path
rather than ongoing cohabitation.

## Parameter Comparison: Libra vs Git vs jj

| Parameter / Flag | Git | jj | Libra |
|---|---|---|---|
| Initialize in current dir | `git init` | `jj git init` | `libra init` |
| Initialize in named dir | `git init <dir>` | `jj git init <dir>` | `libra init <dir>` |
| Bare repository | `git init --bare` | No direct equivalent | `libra init --bare` |
| Initial branch name | `git init -b <name>` / `--initial-branch` | No direct flag (uses `trunk()` revset config) | `libra init -b <name>` / `--initial-branch` |
| Object hash format | `git init --object-format=sha256` | Inherits from Git backend | `libra init --object-format sha256` |
| Template directory | `git init --template=<dir>` | N/A | `libra init --template <dir>` |
| Shared permissions | `git init --shared[=<mode>]` | N/A | `libra init --shared[=<mode>]` |
| Separate storage dir | `git init --separate-git-dir=<dir>` | `jj git init --colocate` | Removed |
| Import from Git repo | N/A (use `git clone --local`) | `jj git init --git-repo <path>` | `libra init --from-git-repository <path>` |
| Vault / signing bootstrap | N/A (manual GPG/SSH setup) | N/A | `libra init --vault <bool>` (default: true) |
| Ref storage format | `git init --ref-format=<format>` (Git 2.45+) | N/A | `libra init --ref-format <format>` |
| Quiet mode | `git init -q` / `--quiet` | N/A | `libra init -q` / `--quiet` |
| Structured JSON output | N/A | N/A | `libra init --json` / `--machine` |
| Recurse submodules | `git init` + `git submodule init` | N/A | N/A (submodules not supported) |

## Error Handling

Every `InitError` variant maps to an explicit `StableErrorCode`.

| Scenario | Error Code | Exit | Hint |
|----------|-----------|------|------|
| Invalid argument (bad branch name, bad format) | `LBR-CLI-002` | 129 | varies by argument |
| Empty or invalid `init.defaultBranch` | `LBR-CLI-002` | 129 | fix the local/global value or use `--initial-branch <name>` |
| Unreadable local/global default config | `LBR-IO-001` | 128 | pass `--initial-branch <name>` to bypass the lookup, or repair the config database named in the error (`libra config --global` cannot repair a repository's local database) |
| Storage root is the Libra home (`~/.libra`) | `LBR-CLI-002` | 129 | "choose a project subdirectory, e.g. `libra init <project>`" |
| `--from-git-repository` on an already-initialized repo | `LBR-CLI-002` | 129 | "convert into a fresh directory instead" |
| Source Git repository not found | `LBR-IO-001` | 128 | -- |
| Source is not a valid Git repository | `LBR-CLI-003` | 129 | "a valid Git repository must contain HEAD, config, and objects" |
| Template directory not found | `LBR-IO-001` | 128 | -- |
| Path is not valid UTF-8 | `LBR-IO-001` | 128 | -- |
| Conversion from Git failed | `LBR-REPO-003` | 128 | -- |
| Vault initialization failed | `LBR-INTERNAL-001` | 128 | Issues URL |
| I/O error (permissions, disk) | `LBR-IO-001` | 128 | -- |
| Database initialization failed | `LBR-INTERNAL-001` | 128 | Issues URL |

## Vault And Identity

- Vault-backed signing is enabled by default
- `--vault false` skips vault setup and writes `vault.signing=false`
- When vault signing is enabled, Libra resolves identity from:
  1. target repository local config
  2. global config
  3. `GIT_COMMITTER_*`, `GIT_AUTHOR_*`, `EMAIL`, `LIBRA_COMMITTER_*`
  4. built-in fallback: `Libra User <user@libra.local>`

This is intentionally less strict than `libra commit`: missing identity does not block repository creation.

## Git Import

`--from-git-repository <path>` fetches objects and refs from a local Git repository and configures
`origin` plus the imported branch layout.

- the source path must point to a valid local Git repository
- `converted_from` in JSON output reports the canonical source Git directory
- empty Git repositories fail with a repo-state error because there are no refs to import

## Compatibility Notes

- `--separate-libra-dir` and `--separate-git-dir` are removed
- non-bare repositories always use the standard `.libra/` layout inside the worktree
- historical repositories that used a `gitdir:` `.libra` link file are no longer detected

Migration for old separate-layout repositories:

```bash
rm .libra
mv /path/to/separate/storage .libra
```
