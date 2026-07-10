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
and (by default) a vault-backed PGP signing key. Non-bare repositories also get a visible
root `.libraignore` file for ignore rules. If `DIRECTORY` is given and does not exist, it is
created first.

When `--from-git-repository` is supplied, objects and refs are imported from the source Git
repository and `origin` is configured to point at the source branch layout. Any `.gitignore`
files found in the source worktree or checked-out import are copied to matching
`.libraignore` files.

Running `libra init` again inside an already-initialized repository is safe: like
`git init`, it re-initializes in place, printing `Reinitialized existing Libra
repository in <path>` and re-creating any missing standard layout (templates,
directories) and re-applying `--shared`, while preserving the existing database —
configuration, `HEAD`, refs, objects, vault, and repository id are otherwise untouched.
`--initial-branch` and `--object-format` are ignored (with a warning) when they
differ from the existing repository, and `--from-git-repository` is rejected on an
already-initialized repository.

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

Override the name of the initial branch. Defaults to `main`. The branch name is validated
against the same rules as `git check-ref-format`: no spaces, no `..`, no ASCII control
characters, maximum 255 characters.

```bash
libra init -b develop
libra init --initial-branch trunk
```

### `--object-format <FORMAT>`

Set the object hash algorithm. Accepted values are `sha1` (default) and `sha256`.

```bash
libra init --object-format sha256
```

### `--from-git-repository <PATH>`

Import objects and refs from an existing local Git repository. The source must contain
valid `HEAD`, `config`, and `objects` structures. An `origin` remote is configured pointing
to the imported branch layout. Empty Git repositories (no refs) produce an error.

For non-bare imports, Libra converts every `.gitignore` it can see into a sibling
`.libraignore`. Existing user-owned `.libraignore` files are preserved and reported as
warnings in structured output.

```bash
libra init --from-git-repository ../old-project
```

### `--vault <BOOL>`

Enable or disable vault-backed PGP signing. Defaults to `true`. When enabled, Libra
generates a PGP signing key during initialization and stores it in the vault. Set to
`false` to skip vault setup entirely.

```bash
libra init --vault false
```

### `--template <PATH>`

Path to a template directory whose contents are copied into the new `.libra` directory.

```bash
libra init --template /path/to/template
```

### `--shared <MODE>`

Specify that the repository is to be shared amongst several users (mirrors the Git
`--shared` flag for group permissions). Supported values are `false`, `umask`,
`true`, `group`, `all`, `world`, `everybody`, or a 4-digit octal mode such as
`0770`.

`true` is canonicalized to `group`; `world` and `everybody` are canonicalized to
`all`. Any explicit `--shared` value is recorded in `core.sharedRepository`, so
`libra config get core.sharedRepository` reflects the active shared mode after a
fresh init or re-initialization.

Numeric modes are validated before any repository layout is written. Libra applies
numeric modes directly to repository directories and files, so the owner must keep
`rwx` access and any group/other class that receives read or write permission must
also receive the matching execute bit for directory traversal. Non-traversable
modes such as `0660` are rejected with `LBR-CLI-002` and do not leave a partial
`.libra` directory.

```bash
libra init --shared group shared-repo
libra init --shared 0770 shared-repo
```

### `--ref-format <FORMAT>`

Set the reference storage format. Accepted values: `strict`, `filesystem`.

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

### `--from-git-repository` instead of Git's lack of import

Git has no built-in concept of importing from another VCS format into itself at init time;
the closest equivalent is `git clone --local`. jj provides `jj git init --git-repo` for
co-located operation with a Git backend. Libra's `--from-git-repository` provides a one-time,
one-directional import that copies objects and refs from a local Git repository into a new
standalone Libra repository. This is a deliberate design choice: rather than wrapping Git
(as jj does), Libra creates a fully independent `.libra` store, making it a standalone VCS
rather than a Git frontend.

### Default branch is `main`, not `master`

Following the industry-wide convention shift, Libra defaults to `main` as the initial
branch name. This can be overridden with `-b` for organizations that use `trunk`, `develop`,
or other naming conventions.

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
| Shared permissions | `git init --shared[=<mode>]` | N/A | `libra init --shared <mode>` |
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
