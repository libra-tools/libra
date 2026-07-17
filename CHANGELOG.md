# Changelog

## [Unreleased]

### Added

- **Crash-safe install transaction and candidate probes (v0.18.99,
  plan-20260714 §A.7)**: new `internal::upgrade::{txn,probe}`. `txn`
  journals the install to `.libra-upgrade-txn.json` through the seven-state
  machine (Prepared → BackupDurable → CandidateInstalled → PostProbePassed →
  Committed, with RollbackIntent/AbortAbsentIntent branches), always writing
  intent before each filesystem mutation and implementing the full §A.7
  recovery decision table so any crash point resolves idempotently to
  committed, rolled-back-to-previous, or aborted-fresh — the post-probe is
  injected so every intermediate on-disk layout is covered by a direct
  reconstruction test. `probe` spawns the candidate/target self-check in its
  own process group with `kill_on_drop` and a hard per-probe timeout,
  killing and reaping the whole group on timeout so no descendant survives;
  any nonzero exit, signal, timeout or spawn failure is a fail-closed probe
  failure. Internal machinery only.

- **Install-directory lock and official-install marker (v0.18.98,
  plan-20260714 §A.2/§A.4/§A.5)**: new `internal::upgrade::{lock,marker}` —
  `InstallDir` opens the install directory once with
  `O_DIRECTORY|O_NOFOLLOW` after §A.5 validation (absolute path, effective-
  uid ownership, no group/world write; no sticky exception granted) and
  performs every target/lock/marker/state operation fd-relative with
  `O_NOFOLLOW` (exclusive-temp + `renameat` + directory fsync atomic writes,
  refusing path separators and dot entries). The advisory `flock` upgrade
  lock uses try-lock (busy ⇒ Skip) for checks and blocking acquire for
  recovery. `.libra-official-install.json` establishes official provenance
  only when the marker parses with `install_source=official_signed_manifest`
  AND its platform/sha256/size match the actual target binary — a marker
  copied next to a foreign binary, or a binary hashing itself, never
  qualifies (§A.2). Non-Unix platforms fail closed (`UnsupportedPlatform`).
  Internal machinery only.

- **Auto-upgrade anti-rollback state and time policy (v0.18.97, plan-20260714
  §A.6/§A.7)**: new `internal::upgrade::state` — durable
  `.libra-upgrade-state.json` (atomic writes, `0600`) recording the highest
  accepted version with per-platform artifact identities, the highest control
  revision with its envelope digest, the monotone `trusted_time_floor`, the
  15-min + deterministic-jitter success cooldown and the ≤1 h failure
  backoff. Pure decision functions enforce: control-revision rollback/fork
  rejection (a pre-revocation envelope cannot replay after a revocation was
  seen), version rollback rejection with same-version artifact-identity
  immutability, required HTTPS `Date` inside the manifest lifetime, expiry
  via `effective_now = max(local, floor, Date)` (clock rollback cannot
  resurrect a manifest; a future local clock only rejects the current round
  and never poisons the floor), floor-anchored cooldown trust windows and
  cache-install refusal when the local clock sits below the floor. Corrupt
  state fails closed (skip upgrading with a warning) instead of silently
  resetting anti-rollback history. Internal machinery only.

- **Dedicated auto-upgrade HTTPS transport (v0.18.96, plan-20260714 §A.6)**:
  new `internal::upgrade::http` — a pinned reqwest client (`https_only`,
  `redirect::Policy::none()` so any 3xx is a hard failure, connect/read
  deadlines), manifest fetch bounded to 1 MiB with the HTTPS `Date` header
  captured for later time policy, effective-URL recheck before any body read,
  and artifact download streaming through a pure `SizeGate` (oversized
  `Content-Length` aborts before the body, per-chunk counting aborts past the
  manifest size, the stream must end at exactly the expected size and match
  the manifest sha256). Internal machinery only; live-server behavioral tests
  land with the `test-upgrade` integration target (§A.11).

- **Signed release-manifest verification core (v0.18.95, plan-20260714 §A.6)**:
  new `internal::upgrade::{manifest,trusted_keys,platform}` — a pure
  `verify_envelope_bytes` implementing the full §A.6 order (envelope parse
  with duplicate-key-id rejection, domain-separated Ed25519 verification via
  `ring`, strict payload semantics: `stable` channel, release-SemVer-only
  versions, exact four-platform artifact matrix with unique platforms,
  structural artifact-URL grammar pinned to
  `https://download.libra.tools/libra/releases/v{tag}/libra-{platform}` with
  `tag == version` and URL-platform == artifact-platform binding, 128 MiB
  size bound, then key-generation floor and key-validity windows). The
  compiled production trust table ships EMPTY until the release-key ceremony,
  so verification fails closed and auto-upgrade stays inert. The new
  `test-upgrade` cargo feature (plus `LIBRA_TEST=1` at runtime) is the only
  trust-root injection path; release builds contain no override code.
  Windows stays published-but-unsupported for auto-upgrade (§A.1). Internal
  machinery only — no CLI surface changes.

- **Reserved `upgrade.mode` config namespace (v0.18.94, plan-20260714 §A.3)**:
  the auto-upgrade switch now lives in `{LIBRA_HOME}/upgrade/settings.json`
  (atomic writes, `0700`/`0600` permissions on Unix), backed by a single
  Rust-side `resolve_libra_home()` that mirrors `install.sh`'s
  `LIBRA_HOME`/`HOME` rules. `libra config` routes every spelling that can
  reach `upgrade.*` through a reserved-namespace router: only single-value
  `set`/`get`/`unset` with `--global` are supported (`unset` resets to `off`
  and keeps the file; missing file reads as `off`; corrupt or unreadable files
  fail with the new `LBR-UPGRADE-001` stable code), `list --show-origin`
  renders the `file:{path}` origin, and local/system scopes, `--add`,
  `--get-all`, `--unset-all`, type conversion, section operations, conflicting
  action-flag combinations, padded spellings, and `--get-regexp` patterns
  matching `upgrade.mode` fail closed as usage errors (`LBR-CLI-002`).
  `config import` skips reserved keys with a warning, and `list` plus
  non-matching `--get-regexp` suppress stale SQLite `upgrade.*` rows. When
  `LIBRA_CONFIG_GLOBAL_DB` isolates the global config database, the upgrade
  settings follow it. The mode itself only selects the upgrade policy
  (`auto`/`manual`/`off`); the upgrade engine lands in follow-up slices.

- **Optional `lba` installer shorthand (v0.18.88)**: `install.sh` now creates
  a movable relative `lba -> libra` symlink by default. Same-version reruns
  repair a missing alias, `--no-alias` and `LIBRA_NO_ALIAS=1` opt out, and
  regular files or foreign symlinks named `lba` are preserved with a warning.
  Symlink-unavailable platforms retain a successful Libra install and receive
  an actionable warning. A deterministic full-installer smoke target covers
  clean install, repair, idempotency, opt-outs, collision safety, and fallback.
- **Reliable format-patch mail output (v0.18.86)**: adds `-1`, `--root`,
  `--minimal`, `--histogram`, `--ignore-if-in-upstream`, and diff-prefix
  controls; honors strict `format.subjectPrefix`, `format.signOff`,
  `format.outputDirectory`, and `format.suffix` defaults with CLI precedence.
  Cover-letter threading now uses unique generated message IDs, full-index is
  effective, complete series render before atomic file writes, and stdout uses
  quiet BrokenPipe handling. A seven-scenario L1 target proves plain and MIME
  Libra→Git `am`, Git→Libra `am`, config, threading, root/diff, and upstream
  patch-id behavior.

- **Minimal mail parsing plumbing (v0.18.85)**: adds repo-independent
  `libra mailinfo <msg> <patch> < mail` with Git-shaped author/email/subject/date
  metadata, body-only message output, separator-through-signature patch output,
  JSON/machine, and quiet modes. `mailinfo` and `am` now share one bounded
  UTF-8 single-part transfer/RFC 2047 parser; repository-specific patch-target
  checks remain in `am`. Both output payloads are staged before per-file atomic
  replacement, and lexical or symlink-parent aliases cannot collapse the two
  destinations. English/Chinese user docs and an eight-scenario Unix
  compatibility target cover repo-free use, folded headers, output safety, and
  fail-closed unsupported inputs.

- **Minimal mail patch sequencer (v0.18.84)**: adds `libra am <patch>...`
  with `--continue`, `--skip`, and `--abort` for bounded plain-text
  `format-patch` mail files. The implementation preserves message/author/date,
  shares the traversal- and symlink-safe text patch engine with `apply
  --check`, pins branch position across recovery, atomically advances
  branch/reflog/sequencer state, and cleans pre-stage new-file remnants on
  abort. English/Chinese user docs and a sixteen-scenario compatibility target
  cover clean-window crash resume plus rollback and document the intentionally
  deferred multipart/binary/3-way/hooks surface.

- **Previous checkout target shortcut (v0.18.83)**: adds worktree-scoped
  `libra switch -` and `libra checkout -` toggling across local branches and
  detached commits. Both commands share HEAD reflog history and record their
  own navigation actions; missing history, deleted source branches, corrupt
  records, and storage failures are rejected before HEAD, index, or worktree
  mutation. English/Chinese user and developer docs plus a nine-case
  compatibility target cover same-command, cross-command, detached, JSON, and
  fail-closed behavior.

- **Import/export fidelity (v0.18.82)**: expands `fast-export` with multiple
  revisions, incremental ranges, `--all`, annotated tags, notes, and Git path
  quoting; expands `fast-import` with inline blobs, copy/rename, annotated tags,
  note records and Git notes-tree translation, reset deletion, bounded parsing,
  object-type validation, and atomic branch/tag/note publication. `bundle`
  gains `--all`/`--branches`/`--tags`, full checksum verification, and bounded,
  hash-kind-aware `unbundle` that imports objects without moving refs. A new
  compatibility target covers Libra round trips, system-Git interoperability,
  transactional failures, repeated unbundle, and SHA-256 repositories; English
  and Chinese command/developer docs describe the supported and deferred edges.

- **Sandboxed repository hooks (v0.18.80)**: adds an Option-A-compatible
  `.libra/hooks` lifecycle for commit, checkout/switch, merge, rebase, and
  pull without executing `.git/hooks`. Hooks run with structured arguments,
  a cleared/allowlisted environment, offline required sandboxing, bounded
  input/output/file sizes, protected repository metadata, blocking pre/message
  semantics, and advisory post-hook warnings. `--no-verify`, command-specific
  pre-hook controls, and `LIBRA_NO_HOOKS` provide documented escape valves;
  English and Chinese repository-hook and command documentation are included.

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

- **Explicit non-sending `send-email` policy (v0.18.87)**: records
  `send-email` as unsupported rather than exposing a misleading transport
  stub. Libra does not read `sendemail.*`, manage SMTP credentials, or contact
  mail servers; users generate interoperable messages with `libra
  format-patch` and validate/send them with stock `git send-email` or another
  mailer. English/Chinese user guidance, the D19 governance decision, and a
  compatibility guard pin the no-network boundary.
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
