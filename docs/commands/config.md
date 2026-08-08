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
libra config generate-ssh-key --remote <name>
libra config generate-gpg-key [--name <name>] [--email <email>] [--usage <usage>]
```

Git-compatible flag style is also supported (hidden from help):

```
libra config [--get | --get-all | --unset | --unset-all | -l | --add | --import | --get-regexp | --show-origin] [--local | --global | --system] [-z | --null] [--type <t> | --bool | --int | --path] [key] [value] [-d <default>]
libra config --remove-section <name>
libra config --rename-section <old-name> <new-name>
```

## Description

`libra config` reads and writes configuration values across three scopes: **local** (repository-level, stored in `.libra/libra.db`), **global** (user-level, stored in `~/.libra/config.db`), and **system** (machine-wide, stored in `/etc/libra/config.db`; lowest cascade precedence, plain config only — no vault). Each database uses SQLite with a `config_kv` table.

Unlike Git's plaintext INI files or jj's TOML files, Libra stores configuration in a transactional database with integrated vault encryption. Sensitive values (API keys, tokens, SSH private keys) are automatically encrypted at rest using AES-256-GCM.

The command supports two invocation styles:

1. **Subcommand style** (preferred): `libra config set key value`, `libra config get key`
2. **Git-compatible flag style** (hidden): `libra config --get key`, `libra config key value`

When reading a value with `get`, Libra cascades through scopes in precedence order: local, then global, then system. The first match wins; an unreadable system database is skipped.

### Bare `libra config <key>`

A single positional argument with no value is a **read**, matching `git config <key>`. It prints the stored value and exits 0, returns the **last** value of a multi-valued key, cascades local → global → system exactly as `get` does, and renders an encrypted value as `<REDACTED>` (use `config get --reveal` for the plaintext). A key that is not set exits **1** with `LBR-CLI-002`. `-z`/`--null` applies here exactly as it does to `get`, terminating the value with NUL instead of a newline.

**Intentional difference from Git:** for a *protected* key — one Libra classifies as a secret (`vault.env.*`, `auth.token.*`, `*.privkey`, or a last segment containing `secret`, `token`, `password`, `credential`, `apikey`, `accesskey`, `privatekey` or `secretkey`) — the bare form keeps Libra's interactive secure-assignment path: it prompts for a **new** value with echo off instead of printing the stored one. With no terminal available it reports `missing value for protected key '<key>' (non-interactive environment)` and exits 2. Read a protected key with `libra config get <key>`, which returns `<REDACTED>`. This divergence is registered in `COMPATIBILITY.md`.

## Options

### Subcommands

#### `set <key> [<value>]`

Set a configuration value. If `<value>` is omitted and the key is sensitive, Libra prompts for interactive input (hidden echo). In non-interactive contexts (CI/CD), use `--stdin` to pipe the value.

| Flag | Description |
|------|-------------|
| `--add` | Add as an additional value for the key, allowing duplicates (like Git's multi-valued keys such as `remote.origin.fetch`) |
| `--encrypt` | Force vault encryption even if the key does not match sensitive-key heuristics |
| `--plaintext` | Force plaintext storage, skipping auto-encryption even for sensitive-looking keys |
| `--stdin` | Read the value from stdin instead of a positional argument (useful for piping secrets in CI/CD) |

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

#### `path`

Print the filesystem path of the config database for the active scope.

```bash
# Show local config path
libra config path
# Output: /path/to/repo/.libra/libra.db

# Show global config path
libra config path --global
# Output: /home/user/.libra/config.db
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

### Scope Flags

These flags are global (apply to any subcommand):

| Flag | Description |
|------|-------------|
| `--local` | Use repository config (`.libra/libra.db`). This is the default for writes. |
| `--global` | Use global user config (`~/.libra/config.db`). |
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

## Scope

- Default scope is local (`.libra/libra.db`)
- `--global` uses `~/.libra/config.db`
- `--system` uses `/etc/libra/config.db` (override with `LIBRA_CONFIG_SYSTEM_DB`); lowest cascade precedence, writes usually need elevated privileges, and vault-encrypted secrets are rejected in this scope (see Design Rationale)

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
