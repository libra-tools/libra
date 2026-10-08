# `libra config`

`libra config` manages repository-local and user-global configuration stored in SQLite-backed
`config_kv`, including vault-backed secrets and key management.

**Alias:** `cfg`

## Synopsis

```
libra config <subcommand> [options]
libra config set [--global | --system] [--add] [--encrypt] [--plaintext] [--stdin] <key> [<value>]
libra config get [--global | --system] [--all] [--reveal] [--regexp] [-d <default>] <key>
libra config list [--global | --system] [--name-only] [--show-origin] [--vault] [--ssh-keys] [--gpg-keys]
libra config unset [--global | --system] [--all] <key>
libra config import [--global]
libra config path [--global | --system]
libra config doctor --global-schema
libra config generate-ssh-key --remote <name>
libra config generate-gpg-key [--name <name>] [--email <email>] [--usage <usage>]
libra config import-gpg-key [--list] [--key <fpr>] [--file <path>] [--passphrase-file <path>] [--replace]
libra config export-gpg-key [--fingerprint] [--out <path>]
libra config remove-gpg-key [--force]
```

Git-compatible flag style is also supported (hidden from help):

```
libra config [--get | --get-all | --unset | --unset-all | -l | --add | --import | --get-regexp | --show-origin] [--local | --global | --system] [-z | --null] [--type <t> | --bool | --int | --path] [key] [value] [-d <default>]
libra config --remove-section <name>
libra config --rename-section <old-name> <new-name>
```

## Description

`libra config` reads and writes configuration values across three scopes: **local** (repository-level, stored in `.libra/libra.db`), **global** (user-level, stored in `<XDG_CONFIG_HOME or ~/.config>/libra/config.db`; an existing legacy `~/.libra/config.db` is copied there automatically on first use and kept as a backup), and **system** (machine-wide, stored in `/etc/libra/config.db`; lowest cascade precedence, plain config only — no vault). Each database uses SQLite with a `config_kv` table.

Unlike Git's plaintext INI files or jj's TOML files, Libra stores configuration in a transactional database with integrated vault encryption. Sensitive values (API keys, tokens, SSH private keys) are automatically encrypted at rest using AES-256-GCM.

The command supports two invocation styles:

1. **Subcommand style** (preferred): `libra config set key value`, `libra config get key`
2. **Git-compatible flag style** (hidden): `libra config --get key`, `libra config key value`

When reading a value with `get`, Libra cascades through scopes in precedence order: local, then global, then system. The first match wins; an unreadable system database is skipped.

### `core.objectformat` is fixed at init

Local-scope writes cannot change `core.objectformat` after the repository is
created (ADR-B3-01). Every mutating spelling refuses with `LBR-CLI-002`
(exit 129): `set`, bare positional assignment, `--add`, `--unset`,
`--unset-all`, `--remove-section core`, `--rename-section` involving `core`,
and `import` when the imported Git config contains the key (case-insensitive
variable match such as `core.ObjectFormat`). The refusal is atomic for
import — no other keys from that import land.

Recreate the repository with `libra init --object-format <sha1|sha256>` to
choose a different format (`blake3` opens later). Init and reinit still write
the key through the database layer; only the `config` command surface is
gated. A `core.objectformat` row under **global** or **system** scope is not
consumed by repository commands and is unchanged by this guard.

### Bare `libra config <key>`

A single positional argument with no value is a **read**, matching `git config <key>`. It prints the stored value and exits 0, returns the **last** value of a multi-valued key, cascades local → global → system exactly as `get` does, and renders an encrypted value as `<REDACTED>` (use `config get --reveal` for the plaintext). A key that is not set exits **1** with `LBR-CLI-002`. `-z`/`--null` applies here exactly as it does to `get`, terminating the value with NUL instead of a newline.

**Intentional difference from Git:** for a *protected* key — one Libra classifies as a secret (`vault.env.*`, `auth.token.*`, `*.privkey`, or a last segment containing `secret`, `token`, `password`, `credential`, `apikey`, `accesskey`, `privatekey` or `secretkey`) — the bare form keeps Libra's interactive secure-assignment path: it prompts for a **new** value with echo off instead of printing the stored one. With no terminal available it reports `missing value for protected key '<key>' (non-interactive environment)` and exits 2. Read a protected key with `libra config get <key>`, which returns `<REDACTED>`. This divergence is registered in `COMPATIBILITY.md`.

## Configuration schema compatibility

GlobalConfig and SystemConfig use the configuration-owned
`configuration_schema_versions` ledger, separate from Repository migrations.
Known Repository-only receipts (including `2026090801` in the current manifest)
do not make a configuration store future. Unknown or mismatched receipts and
true configuration future schemas remain unsupported; remote/cloud commands
that need the affected scope fail closed with `LBR-CONFIG-001`.

An explicit global/system configuration mutation writes the
configuration-owned legacy-reader barrier and its configuration changes in the
same transaction. The barrier is a reserved receipt in the legacy ledger so
older binaries, including the pinned `0.22.16` reader, refuse to write this
store. This build recognizes the exact barrier together with a valid
configuration base receipt. A failed transaction rolls back both the barrier
and the configuration change. Existing legacy receipts are retained.

Scoped get/list, default-value cascades and remote preflight never write the
barrier. Cascaded configuration reads are read-only and do not bootstrap a
missing store. The compatibility transition is forward-only: older binaries
must upgrade; never delete receipts or edit SQLite to force a downgrade.
Recognizing a supported Repository receipt is not permission for automatic
repair. Unknown/unsupported state is upgrade-only in this release.

GlobalConfig uses `LIBRA_CONFIG_GLOBAL_DB` or the XDG configuration directory
(`$XDG_CONFIG_HOME/libra/config.db`, defaulting to `<home>/.config/libra/config.db`
on every platform). While an existing legacy `<home>/.libra/config.db` is the
only store present it stays the active file, so reads and writes never split
across two databases; the first command that actually reads or writes global
configuration copies it to the XDG path once (see
[First-use migration of the legacy global database](#first-use-migration-of-the-legacy-global-database)).
`LIBRA_CONFIG_GLOBAL_DB` is a verbatim override and disables the XDG default,
the legacy fallback and the migration.
SystemConfig uses `LIBRA_CONFIG_SYSTEM_DB` or `/etc/libra/config.db`.
Complete process/repo-local storage configuration may make GlobalConfig
unnecessary, but does not bypass SystemConfig compatibility for remote/cloud
defaults. Use `--offline` or `LIBRA_READ_POLICY=offline|local` only for
intentional local-only object access, not to bypass remote synchronization
safety checks.

## First-use migration of the legacy global database

Releases before the XDG layout stored user-level configuration in
`<home>/.libra/config.db`. When that file exists and
`<XDG_CONFIG_HOME or ~/.config>/libra/config.db` does not, the first command
that actually reads or writes global configuration migrates it, exactly once:

1. the configuration directory is created (`0700` on Unix) and serialized with
   a `.config.db.migrate.lock` advisory lock, so concurrent commands migrate at
   most once;
2. the legacy file is opened **read-only** and copied with SQLite's
   `VACUUM INTO` into `config.db.migrate.<pid>.<nonce>.tmp` inside the target
   directory — a consistent snapshot that folds in any WAL contents;
3. the snapshot must pass `PRAGMA integrity_check` and carry exactly the legacy
   migration receipts and `config_kv` row count;
4. the snapshot is published with a same-directory `rename` and tightened to
   `0600` on Unix.

No schema migration happens: the copy carries the legacy
`configuration_schema_versions` receipts verbatim.

The legacy file is never renamed, modified or deleted — it stays as a downgrade
backup. Nothing reads it once the new path exists, and you can delete it when
you no longer need to run an older release. `libra config path` and
`libra config doctor --global-schema` are diagnostics: they report
`migration_pending` but never migrate.

Verify the result:

```bash
libra --json config path --global
# "path": "/home/user/.config/libra/config.db", "source": "home",
# "migration_pending": false, "legacy_exists": true
```

If the migration cannot run — an unwritable configuration directory, or a
legacy file that is not a Libra configuration database — the legacy database
stays in use:

- **reads** continue and print one `warning:` naming the target path, the
  reason and the `LIBRA_CONFIG_GLOBAL_DB` escape hatch;
- **writes** fail closed with `LBR-IO-002` instead of writing to a database
  that is about to be superseded.

### The global vault key moves with it

The global vault unseal key — the AES-256-GCM key that encrypts `vault.*` and
`auth.token.*` values — lives beside the database it protects, at
`<XDG_CONFIG_HOME or ~/.config>/libra/vault-unseal-key` (`0600`, in a `0700`
directory). A pre-XDG `~/.libra/vault-unseal-key` is copied there on first use
and then kept, unmodified, as a backup; the key content never changes, so every
value encrypted before the move still decrypts after it.

The key is never rotated to work around a problem:

- if both files exist with **different** contents, Libra fails closed and names
  both paths — picking one would make the values encrypted with the other
  unreadable;
- an unreadable or malformed key file is an error, not a reason to generate a
  replacement.

Per-repository key material (`~/.libra/vault-keys/<repo-id>`) and the vault
temp directory (`~/.libra/tmp`) are repository state, not user configuration:
they stay in the Libra home.

To downgrade to a release that predates the XDG layout, copy the migrated
database and key back first (idempotent — the legacy files are still there):

```bash
cp "${XDG_CONFIG_HOME:-$HOME/.config}/libra/config.db" ~/.libra/config.db
cp "${XDG_CONFIG_HOME:-$HOME/.config}/libra/vault-unseal-key" ~/.libra/vault-unseal-key
```

## Read-only global schema doctor

Run `libra config doctor --global-schema` (or `libra --json config doctor --global-schema`)
to inspect only global schema metadata. It works outside a repository, uses the
resolved global config path (env override, XDG default, or the legacy fallback),
and does not read configuration
values, open the vault, inspect System/Repository databases, migrate a schema,
write a barrier, create a backup, or run automatic upgrade/recovery. A missing
target remains absent. `--global` is optional and redundant; `--local`,
`--system` and value/action flags are rejected. The paired `--repair --confirm`
options select the separate mutating workflow below; either option alone fails.

The JSON envelope's `data.report_version` is `1`. The same report backs human
output: `scope`, `role`, `path_source`, configured/canonical paths, `exists`,
`size_bytes`, `modified_at_utc`, `legacy_path`, `legacy_exists`,
`migration_pending`, and `configuration`/`legacy` ledger metadata.
`path_source` is `LIBRA_CONFIG_GLOBAL_DB` (env override), `xdg` (absolute
`XDG_CONFIG_HOME`), `home` (the `<home>/.config/libra` default), or `legacy`
(the old `<home>/.libra/config.db` is still active; `migration_pending` is then
true and `legacy_path`/`legacy_exists` name the fallback file). The doctor is a
diagnostic: it reports a pending migration but never performs one. After the
migration `legacy_exists` stays true while the backup file is on disk, and the
hints say it is an unused backup.
Each ledger reports `observed_version`, `latest_version`, `readable`, and
`verified_name`; versions are strings (including the barrier's `i64::MAX`) to
avoid JSON number precision loss. Null metadata means absent or unavailable,
not proof of a healthy store. Receipt names are displayed only after manifest
validation; arbitrary receipt text and configuration values are never output.

`classification` is `absent`, `compatible`, `upgrade_required`,
`unsupported_future`, `unsupported_receipt`, `unreadable`, or
`changed_during_inspection`. Diagnosis itself exits successfully even when the
store is unsupported; automation must inspect the classification, not just the
exit status. `issue` identifies a proven unsupported ledger/version without
untrusted text. Invalid invocations still fail with the existing CLI usage error.

`producer_disposition` distinguishes registered but unattested Repository
receipts, a recognized configuration barrier, and unattributed state. The
current manifest recognizes `2026090801` as `operation_v2_branch_convergence`;
that does not prove which process/binary wrote this file. Mtime is diagnostic
metadata, not producer attestation. **The default doctor's `repair_eligible`
is always `false`**, including compatible/known receipts. Upgrade to a
producer-compatible build for unsupported state; never manually edit SQLite
receipts. Doctor does not provide a remote-sync bypass.

The reader uses a normal read-only SQLite snapshot, never `immutable` on a live
database. It conservatively reports `unreadable` without opening SQLite when a
WAL-mode file lacks regular WAL/SHM sidecars. Do not create those files manually;
retry while the owning application maintains its normal sidecars, or diagnose
an independently obtained SQLite-consistent snapshot. Existing DB/WAL contents
and modification times are unchanged on a stable target. Before/after file
identity, size and mtime checks report observed concurrent changes as
`changed_during_inspection`. This is not a filesystem lock: external rotation
can race those checks and SQLite coordination files may change. OS access times
and SHM coordination are not invariant, and no repair authority follows from
passing the checks.

## Confirmed legacy global schema repair

Requires Libra v0.22.26 or later; v0.22.25 provides only the read-only doctor.

The opt-in repair workflow is separate from the read-only doctor:

```sh
libra --json config doctor --global-schema
libra --json config doctor --global-schema --repair --confirm /absolute/canonical/path/config.db
```

Use the exact `canonical_path` from your own report, not the example path.
`--confirm` must be absolute and match byte-for-byte; symlink/noncanonical
configured paths and combinations with value operations or another scope are
refused. No repository preflight, system database access or auto-upgrade runs.

Repair currently supports only a narrowly registered **v0.22.19 Linux amd64
producer-format cohort**, not every database with receipt `2026090801`. Its
source revision is `b94bfe12f2ec2f039b88ddb5c6f8871787d60f17` and producer binary
SHA-256 is `03447eb983178433425b5afffba351edae4044e7ded5b9e35c956dddb2bb68a6`.
All 293 schema objects (including SQLite internal structures), all 60 original
receipts and the runtime manifest must match. Repository data or storage paths,
unknown tables/triggers/receipts, and altered bootstrap metadata are ineligible.
Format attestation does **not** identify the historical writer/PID of your file.
Unknown states remain upgrade-only; never change receipts to make a file match.

Mutating repair is **Unix-only** with verified local filesystem semantics:
Linux ext-family, XFS, Btrfs, tmpfs and overlayfs; macOS APFS/HFS. Windows,
other/unrecognized filesystems and network filesystems fail closed before any
repair side effect. The file and its immediate directory must belong to the
current effective user and must not be group/world-writable; the file must have
one hard link. Unsafe ancestors and sidecars are refused. A fixed private
`config.db.schema-repair.lock` serializes repairs and is intentionally retained.
These checks do not defeat a malicious same-user/root process. Stop other
writers and file replacement/rotation tools before repair; observed replacement
or concurrent commits abort the operation.

Before schema changes, SQLite `VACUUM INTO` creates a consistent logical backup
without a source write transaction. It is flushed and reopened for integrity
and format checks. A private `.libra-config-repair-<random>/` directory beside
the target retains `backup.sqlite` (0600) and `recovery.json` (inside a 0700
directory). The application does not enumerate/decrypt configuration values;
SQLite copies their logical contents. Treat the backup as sensitive. Failed or
interrupted copies are retained **unverified**, not silently deleted or reused.

After backup verification, a SQLite write lock protects a second manifest,
attestation and file-identity check. A connection-local nonce and `data_version`
reject a changed connection or a concurrent commit. One transaction initializes
`configuration_schema_versions` and appends the configuration-owned
`configuration_legacy_reader_barrier` to the legacy ledger. Original receipts,
configuration rows, encryption flags and sequence high-water marks are retained;
no Repository migration runs, and no journal-mode change or explicit checkpoint
is requested. Old Repository-only binaries refuse the barrier before writing;
use a compatible new binary thereafter. There is no automatic downgrade.

Successful JSON uses `data.action="repair"`, `report_version=1`,
`outcome="repaired"`, `backup_path`, `backup_verified=true`, and `committed=true`,
plus the registered format/source hashes. An already protected configuration
returns `outcome="already_protected"` with no backup, mutation or producer
attestation; this is not a full database health assessment. Default doctor
output and its `repair_eligible=false` remain unchanged.

Recovery is explicit, never automatic. Preserve both the current database and
its recovery directory. A valid `recovery.json` with `backup_verified=true` is
required before considering `backup.sqlite`; interrupted/missing/unverified
metadata is not proof of a usable backup. After a crash or uncertain commit,
run read-only doctor first: a stale status file cannot determine whether the
transaction committed. Stop every process using the database, verify the
backup's integrity and provenance, preserve the current DB and its sidecars for
forensics, then have the repository maintainer restore that verified consistent
backup as a unit with correct private permissions. Never overlay a live DB,
mix old WAL/SHM files with a restored main file, restore an unverified artifact,
or manually edit migration receipts. Keep a producer-compatible binary available.

Invalid confirmation uses `LBR-CLI-002`; unsupported format/path/permission
eligibility uses `LBR-CONFIG-001`; lock, backup and transaction failures use
`LBR-IO-002`. Failures never include configuration values or raw SQLite schema
errors. A retained backup is not proof that repair committed; check the reported
commit state and rerun diagnosis when the outcome is uncertain.

## Options

### Subcommands

#### `set <key> [<value>]`

Set a configuration value. If `<value>` is omitted and the key is sensitive, Libra prompts for interactive input (hidden echo). In non-interactive contexts (CI/CD), use `--stdin` to pipe the value.

Keys are literal: `_`, `%`, and `\` do not select neighboring keys. For example,
setting `custom.cache%name.value` leaves `custom.cache_name.value` unchanged.
An exact spelling is updated in place; without `--add`, an exact key with
multiple values is refused. When no exact spelling exists, `set` retains its
ASCII case-insensitive replacement behavior. Both exact updates and
case-equivalent replacements preserve an existing encryption flag.

| Flag | Description |
|------|-------------|
| `--add` | Add as an additional value for the key, allowing duplicates (like Git's multi-valued keys such as `remote.origin.fetch`) |
| `--encrypt` | Force vault encryption even if the key does not match sensitive-key heuristics |
| `--plaintext` | Store a new key as plaintext even if it looks sensitive; rejects replacement of existing encrypted values and vault internal/secret keys |
| `--stdin` | Read the value from stdin instead of a positional argument (useful for piping secrets in CI/CD) |

When `set` replaces an ordinary encrypted key, it keeps the value encrypted using
the vault for the selected local or global scope, including ASCII case fallback.
An exact key takes precedence over other spellings; multiple exact values still
require `unset-all` before a single-value assignment. `--plaintext` cannot replace
an existing encrypted value and fails with `LBR-REPO-003` (exit 128), leaving its
rows unchanged. To deliberately store a new plaintext value for an ordinary key,
first run `libra config unset --all [--local | --global] <key>` using the stored
spelling and scope, then set the value again with `--plaintext`. New ordinary keys
still accept plaintext; vault internal/secret keys and system-scope encrypted
values remain rejected. This does not repair values damaged by an older writer.

```bash
# Basic set
libra config set user.name "Jane Doe"

# Set global config
libra config set --global user.email "jane@example.com"

# Force encryption
libra config set --encrypt custom.api_token "sk-abc123"

# Set from stdin (CI/CD)
echo "$SECRET" | libra config set --stdin vault.env.GEMINI_API_KEY

# Add multi-value key
libra config set --add remote.origin.fetch "+refs/heads/*:refs/remotes/origin/*"

# Sensitive key prompts interactively when value omitted
libra config set vault.env.GEMINI_API_KEY
```

#### `get <key>`

Retrieve a configuration value. Cascades local → global → system scope, returning the first match (an unreadable system database is skipped).

| Flag | Description |
|------|-------------|
| `--all` | Return all values for this key (multi-valued keys) |
| `--reveal` | Show the actual decrypted value for encrypted entries (blocked for internal vault credentials like `vault.roottoken_enc`) |
| `--regexp` | Treat `<key>` as a regex pattern and return all matching entries |
| `-d`, `--default <value>` | Return this value if the key is not found (instead of an error) |

```bash
# Simple get
libra config get user.name

# Get with default fallback
libra config get -d "unknown" user.name

# Get all values for a multi-value key
libra config get --all remote.origin.fetch

# Reveal an encrypted value
libra config get --reveal vault.env.GEMINI_API_KEY

# Regex search
libra config get --regexp "user\\..*"
```

#### `list`

List all configuration entries in the active scope.

| Flag | Description |
|------|-------------|
| `--name-only` | Show only key names, not values |
| `--show-origin` | Prefix each entry with its scope (`local`, `global`, or `system`) |
| `--vault` | Show only `vault.env.*` entries |
| `--ssh-keys` | Show SSH key entries |
| `--gpg-keys` | Show GPG key entries |

```bash
# List all local entries
libra config list

# List with scope labels
libra config list --show-origin

# List only vault environment entries
libra config list --vault

# List only key names
libra config list --name-only

# List SSH keys
libra config list --ssh-keys
```

#### `unset <key>`

Remove a configuration entry.

| Flag | Description |
|------|-------------|
| `--all` | Remove all values for this key (for multi-valued keys) |

```bash
# Remove a key
libra config unset user.signingkey

# Remove all values for a multi-valued key
libra config unset --all remote.origin.fetch
```

#### `import`

Import configuration from the user's Git config (`.gitconfig`). Copies relevant entries into Libra's config database.

```bash
# Import from Git global config into Libra global config
libra config import --global

# Import into local config
libra config import
```

If an imported entry would replace an existing encrypted ordinary value with plaintext (for example a key that was stored with `--encrypt` but is not auto-detected as sensitive), the import stops at that key with `LBR-REPO-003` (exit 128) and the same recovery hint as `config set`, naming the key; entries the run had already written are kept and the remaining entries are not imported. Clear every stored spelling of that key with `config unset --all` and re-run the import.

#### `path`

Print the filesystem path of the config database for the active scope.

```bash
# Show local config path
libra config path
# Output: /path/to/repo/.libra/libra.db

# Show global config path
libra config path --global
# Output: /home/user/.config/libra/config.db
```

#### `edit`

Not supported. Libra uses SQLite storage, which cannot be safely round-tripped through a text editor. See [Design Rationale](#design-rationale-why-different-from-gitjj) for details.

#### `generate-ssh-key --remote <name>`

Generate an SSH key pair for the named remote. The private key is stored encrypted in the vault (`vault.ssh.<remote>.privkey`); the public key is stored at `vault.ssh.<remote>.pubkey`.

```bash
libra config generate-ssh-key --remote origin
libra config get vault.ssh.origin.pubkey
```

#### `generate-gpg-key`

Generate a GPG key pair for commit signing or encryption.

| Flag | Description |
|------|-------------|
| `--name <name>` | User name for the key (defaults to `user.name` config) |
| `--email <email>` | User email for the key (defaults to `user.email` config) |
| `--usage <usage>` | Key usage: `signing` (default) or `encrypt` |

```bash
# Generate signing key
libra config generate-gpg-key

# Generate encryption key with explicit identity
libra config generate-gpg-key --name "Jane Doe" --email "jane@example.com" --usage encrypt

# Retrieve the public key
libra config get vault.gpg.pubkey
```

#### `import-gpg-key`

Import an existing OpenPGP signing key from the local GnuPG home or an armored file, enabling commit/tag/merge signing with that identity.

| Flag | Description |
|------|-------------|
| `--list` | List discoverable secret keys from the GnuPG home and exit (no writes) |
| `--key <fpr>` | Select the key to import by fingerprint/key id (required when multiple candidates) |
| `--file <path>` | Import a secret key from an armored file instead of the GnuPG home |
| `--passphrase-file <path>` | Read the key passphrase from a file (required for protected keys in non-interactive use) |
| `--replace` | Replace the active key, archiving the current public key into history first |

**`libra init` already mints an active signing key** (`source: generated`, `vault.signing=true`), so adopting your own key on a default repository requires `--replace`; without it the import fails closed with `LBR-CONFLICT-002` ("an active GPG key already exists; pass `--replace`"). The replaced key's public half is archived, so signatures it already made keep verifying.

```bash
libra config import-gpg-key --list                                   # discover candidates (no writes)
libra config import-gpg-key --key ABCDEF... --replace                # adopt one by fingerprint
libra config import-gpg-key --file my-key.asc --passphrase-file pass.txt --replace
```

#### `export-gpg-key`

Export the active GPG public key. Secrets are never exported.

| Flag | Description |
|------|-------------|
| `--fingerprint` | Print only the primary fingerprint |
| `--out <path>` | Write the armored public key to a file atomically instead of stdout |

Secret material is never exported, and the machine-output flags are refused: `--json`, `--machine` and `--quiet` all fail with `LBR-CLI-002`, so `export-gpg-key` is always plain armored text (stdout or `--out`).

```bash
libra config export-gpg-key            # armored public key to stdout
libra config export-gpg-key --fingerprint
libra config export-gpg-key --out pubkey.asc
```

#### `remove-gpg-key`

Remove the active imported GPG key and fall back to the generated key (never deletes history or generated-key metadata). The removed key's own public half is **archived** first, as `vault.gpg.history.<FPR>.pubkey`, so signatures it already made keep verifying; the result is therefore one extra history row and no other change to the archive. The removal runs as **one transaction**: if any of its four steps fails, the imported key stays active exactly as it was.

**`--force` is required** to remove the active key: without it the command refuses and names the flag (a guard against accidental deletion).

```bash
libra config remove-gpg-key --force
```

Archived public keys live in `vault.gpg.history.<FPR>.pubkey`. Dropping one is an ordinary config unset — `libra config unset vault.gpg.history.<FPR>.pubkey` removes just that fingerprint's snapshot and leaves the other fingerprints alone. **Consequence:** signatures made by the dropped key are no longer accepted by `libra tag -v` or `libra merge --verify-signatures` — that is exactly what the archive exists to prevent, so only drop a row when those signatures no longer matter.

### Scope Flags

These flags are global (apply to any subcommand):

| Flag | Description |
|------|-------------|
| `--local` | Use repository config (`.libra/libra.db`). This is the default for writes. |
| `--global` | Use global user config (`<XDG_CONFIG_HOME or ~/.config>/libra/config.db`; a legacy `~/.libra/config.db` is the active fallback until the first access migrates it). |
| `--system` | Use system-wide config (`/etc/libra/config.db`, overridable via `LIBRA_CONFIG_SYSTEM_DB`). Lowest cascade precedence; writing it usually requires elevated privileges. Vault-encrypted secrets are **not** supported in this scope (see Design Rationale). |

### Hidden Git-Compatible Flags

These flags provide backward compatibility with `git config` invocation patterns. They are hidden from `--help`. Most translate to the equivalent subcommand; `--remove-section` / `--rename-section` are flag-only section operations with no subcommand form.

| Flag | Equivalent Subcommand / Behavior |
|------|----------------------------------|
| `--get` | `get <key>` |
| `--get-all` | `get --all <key>` |
| `--unset` | `unset <key>` |
| `--unset-all` | `unset --all <key>` |
| `-l`, `--list` | `list` |
| `--add` | `set --add <key> <value>` |
| `--import` | `import` |
| `--get-regexp` | `get --regexp <key>` |
| `--show-origin` | `list --show-origin` |
| `--type=<bool\|int\|path>`, `--bool`, `--int`, `--path` | Canonicalize a value when reading (`--get`/`--get-all`/`--get-regexp`) **and when setting**: bool variants → `true`/`false`; int with optional k/m/g (1024-based) multiplier; path expands a leading `~`/`~/`. On a set the value is validated/canonicalized before storage (matching `git config --type`: `yes` → `true`, `1k` → `1024`), and an invalid value errors without storing. A non-get/non-set mode is rejected (exit 129). |
| `--remove-section <name>` | Delete the keys in section `<name>` in one transaction, using Git's section/subsection identity (so `--remove-section branch` removes `branch.<key>` but not the `branch.feature.*` subsection). Missing section → exit 128. |
| `--rename-section <old> <new>` | Move section `<old>`'s keys to `<new>`, preserving each value and its encryption flag. Missing source → exit 128; identical names → exit 2; an already-existing destination section is refused → exit 128. |

### Other Flags

| Flag | Description |
|------|-------------|
| `-d`, `--default <value>` | Default value when key is not found (Git-compat positional mode) |
| `-z`, `--null` | NUL-terminate output records (`git config -z`): `value\0` for `--get`/`--get-all`; `key\nvalue\0` for `--get-regexp`/`--list`; `key\0` with `--name-only`; `origin\0` prefix with `--show-origin`. `--json` takes precedence. Applies to standard config output only; combining it with `--ssh-keys`/`--gpg-keys`/`--vault` is rejected (exit 129). |
| `--json` | Emit structured JSON output |
| `--quiet` | Suppress human-readable output |

## Common Commands

```bash
libra config set user.name "Jane Doe"
libra config get user.name
libra config list
libra config list --show-origin
libra config unset user.signingkey
libra config import
libra config path
```

## Human Output

**`get`** prints the value on a single line:

```
Jane Doe
```

**`list`** prints key-value pairs:

```
user.name=Jane Doe
user.email=jane@example.com
core.editor=vim
```

With `--show-origin`:

```
local   user.name=Jane Doe
global  user.email=jane@example.com
```

With `--name-only`:

```
user.name
user.email
core.editor
```

**`set`** prints nothing on success (exit code 0).

**`path`** prints the database path:

```
/home/user/repo/.libra/libra.db
```

## Structured Output (JSON examples)

**`get`:**

```json
{
  "command": "config",
  "data": {
    "key": "user.name",
    "value": "Jane Doe",
    "origin": "local"
  }
}
```

**`list`:**

```json
{
  "command": "config",
  "data": {
    "entries": [
      { "key": "user.name", "value": "Jane Doe", "origin": "local" },
      { "key": "user.email", "value": "jane@example.com", "origin": "global", "encrypted": false }
    ]
  }
}
```

## Secrets And Vault Entries

Sensitive keys are stored encrypted when they match Libra's sensitive-key rules, including:

- `vault.env.*`
- `*.privkey`
- API keys, tokens, passwords, and similar secret-looking keys

Examples:

```bash
libra config set vault.env.GEMINI_API_KEY
echo "$SECRET" | libra config set --stdin vault.env.GEMINI_API_KEY
libra config set --encrypt custom.api_token "secret"
libra config get vault.env.GEMINI_API_KEY
libra config get --reveal vault.env.GEMINI_API_KEY
libra config list --vault
```

`--reveal` is blocked for internal vault credentials such as `vault.roottoken_enc` and
`vault.ssh.<remote>.privkey`.

## Key Management

SSH keys are generated per remote and stored in config:

```bash
libra config generate-ssh-key --remote origin
libra config get vault.ssh.origin.pubkey
libra config list --ssh-keys
```

GPG public keys are exposed through config, while private signing material stays inside `vault.db`:

```bash
libra config generate-gpg-key
libra config generate-gpg-key --usage encrypt
libra config get vault.gpg.pubkey
libra config list --gpg-keys
```

Supported `--usage` values are `signing` and `encrypt`.

`libra config list --gpg-keys` reports, per entry: its usage, key type, `source` (`imported` or `generated`), fingerprint, signing key id, import time and the count of archived history keys — with secret material redacted (`vault.gpg.seckey_enc` reads back as `<REDACTED>`).

Existing OpenPGP keys can be imported from the GnuPG home or an armored file:

```bash
libra config import-gpg-key --list
libra config import-gpg-key --key ABCDEF...
libra config import-gpg-key --file my-key.asc --passphrase-file pass.txt
libra config export-gpg-key --fingerprint
libra config remove-gpg-key --force
```

The imported secret key is persisted encrypted (`vault.gpg.seckey_enc`) and redacted on every read path; `config get --reveal` refuses it. Signing **fails closed** when neither `vault.gpg.pubkey` nor `vault.gpg.generated_pubkey` is published: the command errors with a recovery hint instead of emitting an unsigned commit/tag. Signing selects the key recorded in `vault.gpg.signing_key_id`, verification uses a fixed allowlist of the active, generated, and historical public keys.

## Scope

- Default scope is local (`.libra/libra.db`)
- `--global` uses `<XDG_CONFIG_HOME or ~/.config>/libra/config.db` (a legacy `~/.libra/config.db` stays the active fallback until the first access migrates it)
- `--system` uses `/etc/libra/config.db` (override with `LIBRA_CONFIG_SYSTEM_DB`); lowest cascade precedence, writes usually need elevated privileges, and vault-encrypted secrets are rejected in this scope (see Design Rationale)

## The `code.defaultProvider` Key

`libra code` resolves its effective provider once at startup; `code.defaultProvider` is the persisted slot in that ladder (explicit `--provider` → `--agent` binding → resumed thread's recorded provider (`--resume`) → **`code.defaultProvider`** → credential detection):

```bash
libra config set --global code.defaultProvider deepseek   # global default
libra config set code.defaultProvider zhipu               # repo-local override
libra config get code.defaultProvider
libra config unset --global code.defaultProvider
```

Valid values are the provider ids accepted by `libra code --provider`: `anthropic`, `codex`, `deepseek`, `gemini`, `kimi`, `ollama`, `openai`, `zhipu`. A configured value skips credential detection; an unset or empty value falls through to it; an unrecognized id makes `libra code` exit 129 (`LBR-CLI-002`) listing the valid ids without echoing the stored value. The key lives in this SQLite config database only — it is unrelated to the `[code.*]` profile sections of `agents.toml` (`[code.multi_agent]`, `[code.goal]`, …), and neither carrier falls back to the other. See [code.md](code.md) for the full resolution ladder.

## The `core.filemode` Key

`core.filemode` (read case-insensitively) controls how `add`,
`update-index <path>`, and `commit -a` record file modes when staging from the
working tree. Unset, it defaults to `true` on Unix and `false` elsewhere. With
`false`, re-staging an existing entry keeps the mode already recorded in the
index and a new path is recorded as `100644` (an executable working-tree file
does not become `100755`); `add --chmod=+x` and `update-index --cacheinfo`
carry an explicit mode and are unaffected. With `true` (the Unix default) a
mode-only worktree change — a tracked regular file whose owner-execute bit
differs from the index while its content is unchanged — is reported by
`status`, rendered by `diff`, staged by `add`/`commit -a`/`update-index
<path>`, and treated as a local modification by `stash push`; with `false`
those commands ignore mode-only differences while entry-type changes (for
example a regular file replaced by a symlink) stay visible. An invalid
boolean value fails `add`/`status` closed with `bad boolean config value
'<value>' for 'core.filemode'` before any index write, mirroring
`commit.verbose`.

```bash
libra config set core.filemode false
libra config get core.filemode
```

## Reserved `upgrade.*` Namespace

The auto-upgrade configuration is a reserved namespace stored in
`{LIBRA_HOME}/upgrade/settings.json` (default `~/.libra/upgrade/settings.json`;
override the base directory with the `LIBRA_HOME` environment variable — and
when `LIBRA_CONFIG_GLOBAL_DB` isolates the global config database, the
settings follow it to that database's directory), never in the SQLite stores.
Only these single-value, `--global` operations are supported:

| Operation | Behavior |
| --- | --- |
| `set --global upgrade.mode <v>` | Accepts `auto`/`manual`/`off` (case-insensitive); anything else is a usage error. Written atomically. |
| `get --global upgrade.mode` | Reads the stored mode; a missing file reads as `off`; a corrupt file is a hard error (`LBR-UPGRADE-001`). |
| `unset --global upgrade.mode` | Resets `mode` to `off` and **keeps** the file. |
| `list --global [--show-origin]` | Renders the file-backed entry with origin `file:{path}`. |

Every other spelling that could reach the namespace fails closed as a usage
error (`LBR-CLI-002`, exit 129): local/system scopes, `--add`, `--get-all`,
`--unset-all`, `--type` conversion, `--encrypt`/`--plaintext`/`--stdin`,
`--remove-section`/`--rename-section`, `--default`, combinations of multiple
action spellings, padded key/value spellings (no whitespace normalization),
and `--get-regexp` patterns that can match `upgrade.mode`. `config import`
skips `upgrade.*` entries with a warning, and `list` plus non-matching
`--get-regexp` patterns suppress any stale `upgrade.*` rows found in SQLite so
the settings file stays the single source of truth. A damaged settings file is
`LBR-UPGRADE-001`.

Resolution order for runtime config-backed environment variables is:

1. CLI arguments
2. Local config (`vault.env.<NAME>`)
3. Global config (`vault.env.<NAME>`)
4. Process environment variables

If no Vault entry or process environment variable supplies a required API key,
Libra reports the missing key and asks you to set `vault.env.<NAME>` or export
`<NAME>`.

## Design Rationale (Why different from Git/jj)

### Why SQLite instead of text files?

Git uses INI-format text files; jj uses TOML. Libra uses SQLite because:

1. **Transactional writes.** SQLite provides ACID guarantees. A crash mid-write cannot corrupt the configuration, unlike a partially-written text file. This is critical when multiple AI agents may write config concurrently.
2. **Structured queries.** Multi-valued keys, prefix searches, and regex matching are SQL queries rather than text parsing. This eliminates an entire class of escaping and parsing bugs.
3. **Integrated encryption.** Vault-encrypted values are stored as encrypted blobs alongside plaintext values in the same table. A text file format would need a separate encryption layer or inline encoding scheme.

### Why vault encryption?

Git stores configurations in plaintext INI files, which is inherently insecure for storing API keys, access tokens, and SSH/GPG private keys. Libra integrates Vault-backed encrypted storage natively. Sensitive keys (like `vault.env.*`, `*.privkey`, or keys containing substrings like `secret`/`token`) are automatically encrypted at rest using AES-256-GCM in both local and global scopes. This eliminates the "redacted in CLI but plaintext on disk" false sense of security, allowing developers to safely store environment overrides directly within the configuration.

### Why does `--system` reject vault-encrypted secrets?

`--system` reads and writes plain system-wide config at `/etc/libra/config.db` (override with `LIBRA_CONFIG_SYSTEM_DB`), at the lowest cascade precedence — like Git's `/etc/gitconfig`. Writing it usually requires elevated privileges, and a present-but-unreadable system DB is skipped during cascade reads rather than crashing other users' commands.

What it deliberately does **not** support is the vault: storing encrypted secrets (`vault.*` keys or `--encrypt` values) in the system scope is rejected with a usage error. In a multi-user OS environment, a system-level unseal key under root-owned `/etc/libra` would either be unreadable to regular users (breaking decryption) or world-readable (defeating the encryption). System-wide *secrets* should be handled at the OS/environment level; Libra keeps the vault to `--global` (user-level) and `--local` (repository) scopes.

### Why no `config edit`?

Libra uses a SQLite database (`config_kv` table) instead of plaintext files. Exporting database rows to a text editor and parsing the unified diff back into SQL `UPDATE`/`DELETE` statements is dangerous. Specifically, for multi-value keys (e.g., `remote.origin.fetch`), the plaintext representation lacks row-level primary keys. Reordered, partially modified, or deleted lines would prevent Libra from accurately mapping text changes to database rows, inevitably leading to data loss or corruption. To guarantee data consistency, you must use the robust `set`, `--add`, `unset`, and `list` commands.

### Why built-in SSH/GPG key management?

Instead of scattering SSH private keys as plaintext files on the filesystem, Libra stores them encrypted inside the config vault (`vault.ssh.<remote>.privkey`). When an SSH transport is invoked, the key is dynamically decrypted to a temporary file (`chmod 600`), passed to the SSH client, and deleted immediately afterward. GPG private keys are managed exclusively by the vault's internal PKI engine and are never exported to the filesystem.

### Why subcommand style as the primary interface?

Git uses `git config key value` (implicit set) and `git config key` (implicit get), which is ambiguous: `git config foo` could be a get or an incomplete set. Libra follows jj's lead by requiring explicit subcommands (`set`, `get`, `list`, `unset`). The Git-compatible flag style (`--get`, `-l`, etc.) is preserved as hidden aliases for migration, but the subcommand style is the documented interface because it is unambiguous, discoverable via `--help`, and easier for AI agents to generate correctly.

### Why `--default` instead of exit-code differentiation?

Git exits with code 1 when a key is not found, which is indistinguishable from other errors in scripts. Libra's `--default` flag provides an explicit fallback value, allowing scripts and agents to handle missing keys without error-code parsing.

## Parameter Comparison: Libra vs Git vs jj

| Feature | Git | jj | Libra |
|---------|-----|-----|-------|
| Implicit set | `git config key val` | No (requires `set`) | `libra config set key val` plus compatible `libra config key val` |
| Subcommand style | No | Yes (`set/get/list/edit/path`) | Yes (`set/get/list/unset/import/path`) |
| Get value | `git config key` | `jj config get key` | `libra config get key` |
| List | `git config -l` | `jj config list` | `libra config list` |
| Edit in editor | `git config -e` | `jj config edit` | Not supported (SQLite storage) |
| Regex search | `git config --get-regexp` | No | `libra config get --regexp` |
| Show origin | `git config --show-origin` | No | `libra config list --show-origin` |
| Type coercion | `--type=bool\|int\|path` | No (TOML types) | `--type=bool\|int\|path` + `--bool`/`--int`/`--path` (canonicalize on both read and set) |
| Default fallback | `--default value` | No | `--default value` |
| Null-delimited | `-z` | No | `-z` / `--null` (`value\0` for get/get-all; `key\nvalue\0` for `--get-regexp`/`--list`; `key\0` with `--name-only`) |
| Rename/remove section | Yes | No | `--remove-section` / `--rename-section` (Git section/subsection semantics; rename refuses an existing destination) |
| JSON output | No | No | **`--json`** |
| Secret redaction | No | No | **Auto-detect** |
| Import from Git | N/A | N/A | **`libra config import`** |
| Vault encryption | No | No | **AES-256-GCM (local/global only; rejected in system scope)** |
| Env var vault | No | No | **`vault.env.*`** |
| SSH key per remote | No | No | **`generate-ssh-key --remote`** |
| GPG key generation | No | No | **`generate-gpg-key`** |
| Env var resolution | No fallback | No fallback | **CLI -> env -> repo -> global** |
| Config file path | N/A | `jj config path` | **`libra config path`** |
| Conditional config | `includeIf` | `[[when]]` blocks | Not supported |
| Worktree scope | `--worktree` | `--workspace` | Not supported |
| Arbitrary file | `--file <path>` | No | Not supported |
| Storage format | INI text files | TOML text files | **SQLite + vault** |
| Scopes | system/global/local/worktree | user/repo/workspace | **system/global/local** (system: plain config only, no vault; no worktree scope) |
| Name-only listing | `--name-only` | No | **`--name-only`** |
| Multi-value add | `--add` | No | **`set --add`** |
| Stdin input | No | No | **`set --stdin`** |
| Force encrypt | No | No | **`set --encrypt`** |
| Force plaintext | No | No | **`set --plaintext`** |

## Error Handling

| Code | Condition | Hint |
|------|-----------|------|
| `LBR-REPO-001` | Not inside a libra repository (for local scope) | Initialize with `libra init` or use `--global` |
| `LBR-CLI-002` | Vault-encrypted secret (`vault.*`/`--encrypt`) in `--system` scope | Use `--global` or `--local` for vault secrets |
| `LBR-CLI-003` | Key not found and no `--default` provided | Check key name with `libra config list` |
| `LBR-CLI-002` | `edit` subcommand used (not supported) | Use `set`, `get`, `unset`, `list` subcommands |
| `LBR-IO-001` | Failed to read config database | Check file permissions on `.libra/libra.db` |
| `LBR-IO-002` | Failed to write config database | Check file permissions and disk space |

## Compatibility Notes

- `libra vault` has been removed. Use `libra config generate-ssh-key`,
  `libra config generate-gpg-key`, and `libra config get vault.*` instead.
- `libra config edit` is not supported (see Design Rationale above).
- Old repositories may still contain legacy `vault.gpg_pubkey` entries; new writes use
  `vault.gpg.pubkey`.

## SSH authentication and captured diagnostics

Libra invokes SSH with `BatchMode=yes` for non-interactive, non-TTY, JSON and
machine callers. In a human terminal with an effective `ask` policy
(`ssh.strictHostKeyChecking` unset or `ask`), a first connection to an unknown
SSH host shows a Git-compatible fingerprint confirmation prompt; accepting it
writes the host key to your OpenSSH known-hosts destination and the original
command continues, while rejecting, EOF or a changed host key fails closed.
Libra never prompts for a private-key passphrase - load or unlock an encrypted
key in `ssh-agent` before retrying. For host trust, verify the fingerprint
through a trusted provider console or another trusted channel before manually
updating `~/.ssh/known_hosts`, or make a separate interactive SSH connection and
compare the displayed fingerprint. For example, `ssh -T git@github.com` uses
GitHub; use the actual repository SSH user, host and port. Do not accept a
fingerprint that has not been verified.

`ssh.strictHostKeyChecking` retains its existing `ask`, `yes`, `accept-new` and
`no` values. The effective policy is resolved in precedence order: a non-empty
`LIBRA_SSH_STRICT_HOST_KEY_CHECKING` environment variable, then local
`ssh.strictHostKeyChecking`, then global, then system, then the default `ask`.
This cascade means a first `clone` outside any repository can adopt an explicit
global or system policy without being forced into the default `ask`.
`ask` leaves that SSH option to the user's SSH configuration;
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
