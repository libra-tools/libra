# Changelog

## [Unreleased]

### Added

- **`libra ls-files` compatibility expansion**: adds `<pathspec>...`
  filtering resolved from the caller's current working directory,
  `--error-unmatch`, and `-z` NUL-delimited text output. The release
  also extends AI/MCP read-only safety coverage for pathspec-based
  inspection and publishes the updated English/Chinese command docs.

- **`libra maintenance` command**: implements Git-compatible `maintenance`
  with subcommands `run`, `register`, `unregister`, and `status`. Supports
  tasks `gc`, `loose-objects`, `pack-refs`, `incremental-repack`,
  `commit-graph`, and `prefetch`. Includes dry-run mode, JSON output, and
  26 integration tests plus 12 unit tests.

- **Cross-cutting `--help` EXAMPLES rollout (v0.17.812..v0.17.836, sealed
  v0.17.837)**: every visible command in `src/cli.rs::Commands` now ends
  its `--help` output with an `EXAMPLES:` section listing the canonical
  invocations. Twenty-five commands grew a `pub const <CMD>_EXAMPLES`
  banner and `#[command(after_help = …)]` wiring: commit, push, merge,
  rebase, reflog, remote, mv, rm, cloud, lfs, usage, publish, grep,
  sandbox, graph, rev-parse, rev-list, symbolic-ref, db, automation,
  code, code-control, hooks, show-ref, agent. Closes
  `docs/development/commands/_general.md` cross-cutting item B.
- **`compat_help_examples_banner` regression guard (v0.17.841)**: spawns
  the libra binary, runs `<cmd> --help` for every visible command,
  and asserts the output contains an `EXAMPLES:` or `Examples:`
  section. Catches future commands that ship without an EXAMPLES
  banner.
- **`compat_command_docs_examples_section` regression guard (v0.17.851)**:
  walks every `docs/commands/<name>.md` page and asserts the body
  contains either an `## Examples` heading or a `## Common Commands`
  heading, keeping the doc surface and the runtime `--help` surface
  in sync.
- **`compat_error_codes_doc_sync` regression guard (v0.17.842)**:
  parses every `LBR-*-NNN` literal out of `src/utils/error.rs` and
  asserts each one appears in `docs/error-codes.md`. Three previously
  undocumented codes (`LBR-ADD-001`, `LBR-AGENT-001`,
  `LBR-UNSUPPORTED-001`) were added in the same patch.
- **`cli::tests::root_after_help_lists_every_visible_command`
  (v0.17.840)**: unit-level guard asserting every non-hidden command
  appears in some Command Groups row of `libra --help`. Closes the
  drift that left `fsck` and `hash-object` ungrouped.
- **`docs/commands/hooks.md` (v0.17.838)** and `docs/commands/README.md`
  Low-Level & Inspection index entry (v0.17.839): completes the
  hidden-plumbing doc coverage (every other hidden command already
  had a page).
- **Documentation Examples sections (v0.17.844..v0.17.850)**: added
  to `docs/commands/automation.md`, `docs/commands/usage.md`,
  `docs/commands/db.md`, `docs/commands/sandbox.md`,
  `docs/commands/publish.md`, `docs/commands/ls-remote.md`, and
  `docs/commands/agent.md` so every per-command doc carries an
  invocation section (enforced by
  `compat_command_docs_examples_section`).

- **`libra fsck`**: Repository integrity checker analogous to `git fsck`. Verifies
  object hash integrity (SHA1/SHA256), object format validity, ref consistency,
  index integrity, and cross-reference validation (including object type mismatch
  detection for tree entries). Supports `--verbose`, `--json`, `--objects-only`,
  `--no-cross-ref-check`, `--no-index-check`, and `--fix` (auto-repair broken refs
  and rebuild corrupted index). Exit codes use a bitmask scheme:
  bit 0 = object corruption, bit 1 = broken refs, bit 2 = index corruption.
- **`docs/commands/fsck.md`**: Comprehensive documentation for the `fsck` command
  including parameter comparison with Git, design rationale, and CI/CD examples.

### Documentation

- **AI provider env constructor policy (v0.17.1048)**: provider
  Rustdocs now define `Client::from_env()` as a source-compatible
  legacy helper for the 0.17 line and `Client::from_resolved_env(...)`
  as the preferred runtime bootstrap for repository/global
  vault-aware config. The v0.18 migration note is explicit:
  `from_env()` will be deprecated but retained for compatibility,
  while new runtime call sites should use `from_resolved_env` with a
  `LocalIdentityTarget`.
- **Root help command groups (v0.17.840)**: `fsck` and `hash-object`
  added to the `Maintenance And Plumbing` row of `libra --help`'s
  Command Groups section. Both commands were callable and documented
  but absent from the scenario-grouped index.
- **Stale src/ file-count claim refreshed (v0.17.843)**: bumped
  410 → 427 in `docs/development/commands/_general.md`'s
  `compat_all_production_unwrap_guard` description.
- **`libra code` Code-phase closeout (C1–C8)**: synced
  `docs/development/tracing/code.md`, `docs/commands/code.md`,
  `docs/commands/zh-CN/code.md`, `COMPATIBILITY.md`, and
  `tests/INDEX.md` to the shipped mode/provider/Web/MCP/session/
  approval behavior. The `run_libra_vcs` allowlist docs now list all
  ten commands (`status`, `diff`, `branch`, `log`, `show`, `show-ref`,
  `ls-files`, `add`, `commit`, `switch`) and recommend `ls-files
  --others --exclude-standard` for untracked-path inspection, matching
  the tool's own guidance.
- **Agent Gate 8 closeout docs (v0.18.21)**: re-audited the Agent
  tracing plan against the shipped code and updated
  `docs/development/tracing/agent.md` / `plan.md` to reflect the
  implemented first-batch roster, hook providers, lifecycle events,
  checkpoint/export/doctor/retention/audit behavior, and intentionally
  deferred parity items. `compat_agent_docs_contract` now also pins the
  schema/retention/raw-export wording and the current internal runtime
  source-of-truth link.
- **Mutating fix bridge deferred (no agent↔code write collaboration
  yet)**: the internal AgentRuntime serialized fix bridge is not
  enabled. `libra review --fix` and `libra investigate fix` stay
  read-only and fail closed with `LBR-AGENT-010`
  (`ERR_AGENT_FIX_BRIDGE_UNAVAILABLE`, exit 128); `libra agent`
  review/investigate produce findings only and never mutate the
  working tree through `libra code`. Because the bridge is unbuilt,
  there is no `libra agent` ↔ `libra code` mutating collaboration
  boundary to describe — findings-to-fix hand-off remains a documented
  deferral until the bridge lands with approval/sandbox/tool-ACL
  coverage.
- **External agent discovery is preview / opt-in (default off)**:
  `libra agent rpc list/trust/invoke` over external `libra-agent-*`
  binaries is disabled by default behind the `agent.external_agents.enabled`
  setting; unknown binaries are quarantined (never registered as
  callable) and built-in slug impersonation is skip-and-logged. This is
  a preview surface — enable it deliberately per repo, it is not on by
  default.
- **D1/R2 deletion propagation for agent-capture data is deferred**: a
  best-effort cloud mirror already exists via `libra cloud sync` — agent
  checkpoint blobs/trees/commits reach R2 through `object_index`, and
  `agent_session` / `agent_checkpoint` rows are mirrored to D1. Local
  erasure (`libra agent clean --gc` and session erasure) rewrites
  `refs/libra/traces` and drops the local DB / `object_index` rows, but
  it does NOT push a tombstone/delete to D1/R2, so a later
  `cloud sync` / restore from another machine could resurrect erased
  agent-capture data. Tombstone/deletion propagation to D1/R2 is
  explicitly deferred until it lands.

## [0.1.6]

### Breaking Changes

- **`libra init --separate-libra-dir` and `--separate-git-dir` removed**: non-bare repositories now always use the standard `.libra/` directory inside the worktree. Historical repositories that still use a `.libra` `gitdir:` link file are no longer detected. Migration:
  ```bash
  rm .libra
  mv /path/to/separate/storage .libra
  ```

### Changed

- **`libra init` execution/render split**: init now uses a silent execution layer internally so `clone` and other callers no longer leak init progress or JSON envelopes.
- **Human progress output**: default `libra init` now reports major phases (`Creating repository layout`, `Initializing database`, `Setting up refs`, Git conversion, vault key generation) on `stderr`.
- **Structured success output**: `libra init` now supports stable `--json` / `--machine` success envelopes with path, branch, object/ref format, repo id, vault state, Git conversion source, and SSH-key detection.
- **Git import cleanup**: `--from-git-repository` now uses the safe fetch path and suppresses nested fetch progress/JSON noise from `stderr`.
- **Vault identity alignment**: init now resolves signing identity from target-local config, global config, and commit-compatible environment fallbacks before using the built-in default identity.
- **Explicit `vault.signing=false`**: `libra init --vault false` now records the disabled signing state in `config_kv` instead of leaving it implicit.
- **Canonical config seeding**: init continues to seed only `config_kv` canonical keys (`core.*`, `libra.repoid`) and no longer relies on legacy `config` table writes.

## [0.1.5]

### Breaking Changes

- **`libra vault` subcommand removed**: Vault functionality has been integrated into `libra config`. Migration guide:
  | Old command | New command |
  |-------------|------------|
  | `libra vault generate-ssh-key` | `libra config generate-ssh-key --remote <remote-name>` |
  | `libra vault generate-gpg-key` | `libra config generate-gpg-key` |
  | `libra vault gpg-public-key` | `libra config get vault.gpg.pubkey` |
  | `libra vault ssh-public-key` | `libra config get vault.ssh.<remote-name>.pubkey` |

  Note: `<remote-name>` should be replaced with your actual remote name (usually `origin`).

- **`--system` scope removed**: System-level configuration has been removed due to multi-user permission isolation issues. Migrate existing `--system` config to `--global`:
  | Old usage | New usage |
  |-----------|----------|
  | `libra config set --system key value` | `libra config set --global key value` |
  | `libra config --get --system key` | `libra config get --global key` |
  | `libra config --list --system` | `libra config list --global` |

- **`libra config edit` not supported**: Libra uses SQLite storage; multi-value key diff-based editing cannot guarantee data consistency. Use `libra config set`/`unset`/`list` to manage configuration.

- **Config storage backend migrated**: Configuration storage moved from three-column split table (`config`) to flat key/value table (`config_kv`) with optional vault encryption. Old `Config` API is deprecated.

### Added

- **Subcommand-style CLI**: `libra config set/get/list/unset/import/path/generate-ssh-key/generate-gpg-key` with Git-compatible flag aliases (`--get`, `--list`, `-l`, `--unset`, `--add`, etc.)
- **Vault-backed encryption**: Sensitive keys (`vault.env.*`, `*.privkey`, API keys, tokens, passwords) are automatically encrypted using AES-256-GCM
- **Environment variable vault**: `vault.env.*` namespace for storing API keys and secrets with `resolve_env()` priority chain (CLI args > system env > local config > global config)
- **Per-remote SSH keys**: `libra config generate-ssh-key --remote <name>` generates isolated SSH keys per remote
- **`--encrypt` flag**: Force encryption for any config value
- **`--stdin` flag**: Read values from stdin for CI/CD pipelines
- **`--show-origin` flag**: Show which scope (local/global) each config value comes from
- **`--vault` flag**: List vault environment variables across scopes
- **`config path` subcommand**: Show config database file path
- **`config import`**: Enhanced with `--no-includes` for global scope, multi-value key handling, auto-encryption of sensitive keys
- **Sensitive key auto-detection**: `is_sensitive_key()` classifies keys by naming patterns
