# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## ⚠️ This repository is a Libra repository — use `libra`, not `git`

This working tree is version-controlled by **Libra**, not Git: its metadata lives in `.libra/` (there is no `.git/`). Run **`libra <command>`** for all version-control operations — `git` commands will not work here.

`libra` is installed on `PATH`. If it is missing locally, install it with:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh
```

Its CLI is largely Git-compatible, so the everyday commands map one-to-one — just swap the binary name:

```bash
libra status              # not: git status
libra add <path>          # not: git add
libra commit -s -m "..."  # not: git commit (-s adds the required DCO trailer)
libra log                 # not: git log
libra diff                # not: git diff
libra branch / switch / checkout / merge / rebase / push / pull / fetch …
```

Compatibility is *partial* and governed by the four-tier matrix in [`COMPATIBILITY.md`](COMPATIBILITY.md) (`supported` / `partial` / `unsupported` / `intentionally-different`) — consult it before assuming a Git flag or subcommand behaves identically. Libra also adds AI-native subcommands with no Git equivalent (`automation`, `sandbox`, `agent`, `review`, `investigate`), plus further Libra-only commands graded `intentionally-different` in the matrix (`upgrade`, `op`, `cache`, `logfile`, `cloud`, `layer`, `sparse-view`, `hydrate`, `file`, `alternates`, `deps`, `metadata`, `revision`, `dirty`, `service`, `auth` / `login` / `logout` / `whoami`, and `media` in `--features fastcdc` builds). `libra --help` lists them (all but `op` also appear in a "Command Groups" row). (`code`, `code-control`, `graph`, `usage` and `publish` were removed in the 0.23.0 breaking release — external-agent capture is `libra agent`, repository backup is `libra cloud`.)

For scripted / agent use every command accepts the global `--json` (or `--json=ndjson`), `--machine`, `--quiet`, `--exit-code-on-warning`, `--no-pager` and `--color=<when>` flags; `libra help error-codes` prints the stable error-code table (`docs/error-codes.md` is compiled into the binary).

(Note: this constraint is about operating *in* this repo. To build/test the Libra source itself, use the `cargo` commands in **Build & Development Commands** below; to run the in-tree build of the CLI use `cargo run -- <command>`.)

## Project Overview

Libra is an **AI agent–native version control system** written in Rust. It partially implements a Git client with full on-disk format compatibility (`objects`, `index`, `pack`, `pack-index`) while using SQLite for transactional metadata (`config`, `HEAD`, `refs`). It is designed for monorepo/trunk-based development with tiered cloud storage (S3/R2) and a Cloudflare D1/R2 backup path.

The former `libra code` Web Code UI and the top-level `libra graph` / `libra usage` / `libra publish` surfaces were removed in the 0.23.0 breaking release; external-agent capture is `libra agent`, read-only `review` / `investigate`, `sandbox` and `automation` remain, and repository backup is `libra cloud`. The Git surface is governed by a four-tier compatibility matrix (`supported` / `partial` / `unsupported` / `intentionally-different`) tracked in [`COMPATIBILITY.md`](COMPATIBILITY.md); AI-only commands (`automation`, `sandbox`, `agent`, `review`, `investigate`, `cloud`) are explicitly Libra-only extensions.

The default `cargo build` embeds no Next.js export (`WebAssets` and its `build.rs` embed were removed in 0.23.x); the `web/` frontend tree and the former Publish Cloudflare Worker (`worker/`) were both fully removed in 0.23.x.

Self-upgrade and release signing: `libra upgrade` updates an official script install over the Ed25519-signed stable channel (`src/internal/upgrade/`, `docs/auto-upgrade.md`, `docs/development/internal/release-signing-auto-upgrade.md`). The `upgrade.mode` switch lives in `{LIBRA_HOME}/upgrade/settings.json`, never in SQLite. The public trust table in `src/internal/upgrade/trusted_keys.rs`, the pinned key constants in `install.sh` / `install.ps1`, and the ceremony record line in `docs/development/internal/release-signing-auto-upgrade.md` are asserted equal by a unit test — a key change must update all four files together.

## Build & Development Commands

### Essential Commands

```bash
# Format code (requires nightly toolchain)
cargo +nightly fmt --all

# Lint — all warnings must be resolved before committing (all features on)
cargo clippy --all-targets --all-features -- -D warnings

# Dependencies compile at opt-level 2 in dev profile (TA-05: kills the 5-17s
# opt-0 RSA keygen in every `libra init --vault`); first build after checkout
# recompiles deps once (+10-20 min), behavior unchanged.
# Quick compile check
cargo check
cargo build

# Run full test suite (L1 only by default; L2/L3 auto-skip when env vars are unset)
cargo test --all

# Faster full suite via cargo-nextest (one process per test; external-resource
# mutual exclusion comes from the generated .config/nextest.toml — regenerate
# with `sh tests/NEXTEST_GROUPS.sh` after touching tests/SERIAL_REGISTRY.tsv).
# The authoritative acceptance gate remains `cargo test --all` (plan-20260827
# ADR-NP-04); nextest is the additional fast execution face. Pinned install:
cargo install cargo-nextest --version 0.9.143 --locked   # verify: cargo nextest --version
cargo nextest run --all

# Local test evidence/driver artifacts (junit archives, batch logs) should go
# to a persistent scratch dir surviving reboots — set LIBRA_TEST_SCRATCH_DIR
# to a directory on durable media (system /tmp is wiped on reboot). This is a
# plan-driver convention only; nothing in-tree reads the variable.

# .cargo/config.toml sets RUST_MIN_STACK=16 MiB for every cargo-launched process
# (debug test binaries overflow the 2 MiB default); the CLI itself runs on a
# 32 MiB thread (src/main.rs).

# Run specific tests (tests/command/*.rs compile into the single `command_test` binary)
cargo test command::init_test
cargo test add_test
cargo test --test command_test add_test

# Run the CLI
cargo run -- <command>          # e.g. cargo run -- status

```

### Cargo Features

| Feature | Purpose |
|---------|---------|
| `worktree-fuse` | Enable Unix FUSE-backed worktree commands (Linux/macOS only) |
| `test-network` | Gate L2 tests requiring outbound network but no secrets |
| `test-live-ai` | Retired in 0.23.x: the Code-era live LLM targets were removed and no `[[test]]` target requires the feature |
| `test-live-cloud` | Gate L3 tests hitting real D1/R2 endpoints |
| `test-live-agent` | plan-20260713 live agent gate: real local `claude`/`codex`/`opencode` CLI data on the dev acceptance machine (requires `LIBRA_RUN_LIVE_AGENT_GATE=1`; missing stores print skipped) |
| `subagent-scaffold` | Schema-only sub-agent contract scaffold (CEX-S2-10, gated on CP-4 in production) |
| `test-upgrade` | plan-20260714 §A.11 auto-upgrade test hooks (trust-root/endpoint injection; needs `LIBRA_TEST=1` at runtime; release builds must never enable it) |
| `otlp` | OTLP trace export (lore.md 1.7): one vetted command-span to an explicitly configured collector; default binary unaffected |
| `fastcdc` | Default-OFF FastCDC LFS media transport (lore.md §6): enables the `libra media` command and `media_fastcdc_test`; must never join `default` (`compat_fastcdc_feature_gate_guard`) |
| `keyring` | OS-keyring auth backend (lore.md 2.7): release builds are built with `--features keyring` (`release.yml`); default dev builds unaffected (`auth_keyring_backend` test) |

### CI Pipeline (`.github/workflows/base.yml`)

`base.yml` runs on `pull_request` only. All PRs must pass these jobs (display names below; the job ids are `format`, `clippy`, `redundancy`, `test`, `network-remotes`, `owner-liveness-macos`, `opencode-export-linux`). Jobs 1–5 run on the `[self-hosted]` runner pool, 6 on `macos-latest`, 7 on `ubuntu-latest`:
1. **compat-rustfmt** — `cargo +nightly fmt --all --check`
2. **compat-clippy** — `cargo clippy --all-targets --all-features -- -D warnings`, then `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --no-deps --all-features`
3. **compat-redundancy** — directory-shape check on `third-party/rust/crates` (passes trivially when the directory is absent, as it is today)
4. **compat-offline-core** — `cargo test --test compat_matrix_alignment compatibility_matrix_matches_cli_commands -- --exact` + pinned `cargo nextest run --all --no-fail-fast --retries 2` (one process per test; external-resource mutual exclusion from the generated `.config/nextest.toml`) + `cargo test --doc` + the `otlp` (`otlp_telemetry`), `keyring` (`auth_keyring_backend`) and `test-upgrade` (`upgrade_auto_test`, `upgrade_publish_contract_test`, with `LIBRA_TEST=1`) feature sections on `cargo test`. Test steps run with `HOME` / `USERPROFILE` / `XDG_CONFIG_HOME` pointed at an isolated `libra-ci-home` and require `rg` (ripgrep) on `PATH`
5. **compat-network-remotes** — `cargo test --features test-network --test network_remotes_test -- --test-threads=1`
6. **compat-owner-liveness-macos** — `cargo test --lib -- claim_owner_tests --test-threads=1` on `macos-latest` (plan-20260714 W1 §C.9 claim-owner liveness proof)
7. **opencode-export-linux** — on `ubuntu-latest`: installs bubblewrap, runs the `internal::ai::observed_agents::opencode_export` lib gates, then `cargo test --test agent_opencode_bridge_test` (fails if the bridge test reports `skipped`)

Additional workflows: `codeql.yml` (security analysis), `live-compat.yml` (scheduled / manual `compat-live-cloud`, not a required check), `release.yml` (release pipeline: runs on `v*` tags only, builds with `--features keyring`, uploads through the OIDC credential broker at `https://libra.tools` with no long-term R2 secrets — `compat_release_rclone_env_guard` — then requests the Ed25519-signed stable manifest).

## Test Layers

Libra tests are organised into three layers — `cargo test --all` runs L1 only; L2/L3 are silently skipped when their env vars are unset. See [`docs/development/integration/integration-test-plan.md`](docs/development/integration/integration-test-plan.md) for the canonical guide.

| Layer | Dependencies | Trigger |
|-------|--------------|---------|
| **L1 — Deterministic** | None (tempdir, in-memory stores, mock models) | `cargo test --all` |
| **L2 — Network** | GitHub token for temporary repo creation | `LIBRA_TEST_GITHUB_TOKEN` + `LIBRA_TEST_GITHUB_NAMESPACE` |
| **L3 — Live Services** | Real cloud credentials (`LIBRA_D1_*`, `LIBRA_STORAGE_*`) | Set the relevant env vars and run with `--features test-live-cloud`; the Wave 4 live-AI targets were retired in 0.23.x |

Gate L2 / L3 tests with the small `env_is_present(name) -> bool` helper (see e.g. [`tests/cloud_storage_backup_test.rs:29`](tests/cloud_storage_backup_test.rs)) followed by an early `eprintln!("skipped (...)")` return when a required var is unset — missing vars print "skipped", never fail. Copy `.env.test.example` → `.env.test` and `source` it before running the full suite (the `export` prefix is required).

## Coding Conventions

### Language & Style

- **Rust edition 2024**, 4-space indentation
- **Naming**: `snake_case` for modules/functions, `PascalCase` for types/traits, `SCREAMING_SNAKE_CASE` for constants
- **Imports**: Grouped as Standard → External → Crate per `rustfmt.toml` (`group_imports = "StdExternalCrate"`, `imports_granularity = "Crate"`); avoid wildcard imports except in tests

### Error Handling

- **CLI flows**: Use `anyhow::Result` for flexible error propagation
- **Library code**: Use `thiserror` with domain-specific error enums (e.g., `InitError`, `GitError`)
- **Command handlers**: `execute(args)` is the public async entry; may return early without Result for simple CLI feedback
- **Database operations**: `_with_conn` suffix for transaction-safe variants accepting `ConnectionTrait`
- **Avoid `unwrap()` / `expect()`**: Prefer returning `Result` and propagating errors with `?`, attaching human-readable context via `.context("...")` or `.with_context(|| format!(...))` so end-users see actionable messages instead of panics. `unwrap()`/`expect()` are acceptable only in **unit/integration tests** and where the logic is **obviously infallible** (e.g., compile-time-known constants) with a brief `// INVARIANT:` comment. All other code — including program startup and initialization — must handle errors gracefully and return actionable messages.
- **User-friendly error messages**: All errors surfaced to the user must be human-readable and actionable. Avoid exposing raw internal errors; wrap them with context that explains *what went wrong*, *which resource was affected* (path, ref, object ID), and *how to fix it*.

### Patterns

- **Command structure**: Each command in `src/command/<name>.rs` with an `Args` struct (clap derive) and `async fn execute(args)`
- **Extension traits**: `TreeExt`, `CommitExt`, `BlobExt` add methods to git-internal types
- **Builder pattern**: Used for `AgentBuilder`, with validation in builder methods returning `Result`
- **Guard pattern (RAII)**: `ChangeDirGuard` for safe directory changes in tests
- **Provider pattern**: Each AI provider has `mod.rs` + `client.rs` + `completion.rs`
- **Global hash-kind preflight**: Before dispatching most object-touching subcommands, `cli.rs` reads `core.objectformat` (defaulting to `"sha1"`, also accepting `"sha256"`) and calls `git_internal::hash::set_hash_kind` so the entire process hashes consistently. New commands that read/write objects must run through this preflight rather than assuming SHA-1 or hard-coding object-ID byte lengths (20 vs 32).

### Documentation

- Module-level `//!` doc comments explaining purpose
- Function-level `///` with `# Arguments`, `# Returns`, `# Example` sections where helpful
- Architecture notes as block comments (`/* ... */`) for complex patterns like `_with_conn`
- Add comments only when control flow is non-obvious (async handling, SQLite migrations)

## Testing Guidelines

- **Integration tests** in `tests/command/` mirror real Git workflows; prefer these for new commands
- **Compatibility-surface tests** in `tests/compat/` guard against regressions in CLI flag/help wording, declined-feature drift, and the production `unwrap()` audit. Each `*.rs` under `tests/compat/` must be registered as a `[[test]]` entry in `Cargo.toml` (Cargo's default discovery only picks up files directly under `tests/`). New compat guards must also add a row to the inventory table in [`tests/compat/README.md`](tests/compat/README.md). See [`docs/development/integration/integration-test-plan.md`](docs/development/integration/integration-test-plan.md) for the full convention.
- **Cross-cutting `--help` EXAMPLES contract**: every visible command in `src/cli.rs::Commands` ships with a `pub const <CMD>_EXAMPLES` constant wired via `#[command(after_help = …)]` (or `after_help = command::<name>::<CMD>_EXAMPLES` on the parent subcommand binding in `cli.rs` for `Subcommand`-style commands). Three compat guards protect this contract: `compat_help_examples_banner` (every `<cmd> --help` renders an EXAMPLES section), `cli::tests::root_after_help_lists_every_visible_command` (every non-hidden command appears in a Command Groups row), and `compat_command_docs_examples_section` (every `docs/commands/<name>.md` page carries an Examples / Common Commands heading). New commands must satisfy all three.
- **Test index**: `tests/INDEX.md` is the authoritative one-line index of every cargo `--test` target (`target | wave | one-line purpose | relevant src`; waves 1 command/compat, 1F feature-gated deterministic, 2 Code UI & local automation, 3 network, 4 live AI, 5 live cloud, 6 perf smoke, 7 local agent capture). Add or update its row when adding or renaming a target; reference cases as `<target>::<test_fn>`
- **Isolation**: Use `tempfile::tempdir()` and `utils::test::ChangeDirGuard` to isolate state
- **Serial execution**: Use keyed lanes — `#[serial(cwd)]`, `#[serial(env)]`, `#[serial(hash_kind)]`, or named external keys such as `cloud_live` / `workspace_failpoints` — for the process-wide resource a test really touches (plain unkeyed `#[serial]` serializes against every other unkeyed test). Every such annotation needs a matching row in `tests/SERIAL_REGISTRY.tsv`; the `compat_serial_registry` guard re-runs `tests/SERIAL_CLASSIFY.sh` and fails on drift. `.config/nextest.toml` is generated from the registry — rerun `sh tests/NEXTEST_GROUPS.sh` after touching it
- **Async tests**: Use `#[tokio::test]` (or `flavor = "multi_thread"` when needed)
- **Fixtures**: Keep small and local in `tests/data/` and `tests/fixtures/` (`tests/compat-ledger/` is the Git-compatibility evidence ledger, one row per migrated upstream scenario, schema-guarded by `compat_ledger_schema` — not a fixture directory); reuse helpers from `tests/command/mod.rs`, `tests/harness/` (local agent-capture harness), and `tests/helpers/`
- **Gating**: Use the `env_is_present(name)` helper pattern (see `tests/cloud_storage_backup_test.rs:29`) plus an early `eprintln!("skipped (set ...)")` return so missing vars print a skip notice and do not fail the test. Match the L1/L2/L3 layering and the matching `test-network` / `test-live-cloud` Cargo features (the `test-live-ai` layer was retired in 0.23.x)
- **Coverage**: Pair new commands/options with at least one end-to-end test plus a focused unit test, and an entry in `COMPATIBILITY.md` if you change the Git surface. New `StableErrorCode` variants must also be added to `docs/error-codes.md` (the `compat_error_codes_doc_sync` test guard fails otherwise).

## Quality Acceptance Criteria (质量验收标准)

A change is considered done only when all three of the following pass locally with no manual fix-ups:

1. **Formatting** — `cargo +nightly fmt --all --check` reports no formatting differences.
2. **Lint** — `cargo clippy --all-targets --all-features -- -D warnings` reports no warnings.
3. **Tests** — `source .env.test && cargo test --all` passes in full (L1 always runs; L2/L3 print "skipped" rather than fail when their env vars are unset — that is acceptable, an actual failure is not).

These mirror the `compat-rustfmt`, `compat-clippy`, and `compat-offline-core` CI jobs, so passing them locally is the precondition for opening a PR. Run all three before reporting work as complete.

### Layered test execution for plan-driven work (ER-13)

When the work is driven by a task-card plan under `docs/development/plan/`, the **test** gate above is layered per [`plan-template.md`](docs/development/plan/plan-template.md) **ER-13**. Formatting and lint are *not* layered — every card that gets pushed still runs both.

- **Task-card execution phase** — run only the tests related to that card: the ER-04 A-group focused commands for the surfaces the card actually touched, plus the card's own `Verification` cases. A full `source .env.test && cargo test --all` is required on a card only when it hits an ER-13 trigger: `T-1` cross-cutting surfaces (`sql/**`, `src/cli.rs` command registration or global flags, stable error codes and `docs/error-codes.md`, shared single-source-of-truth helpers, `build.rs`, non-version `Cargo.toml` lines, `rustfmt.toml`, `.github/workflows/**`, `install.sh` / `install.ps1`), `T-2` release / aggregation cards, `T-3` removals or renames of public surfaces, `T-4` shared test infrastructure (`tests/harness/`, `tests/helpers/`, `tests/command/mod.rs`), `T-5` an explicit request to run the full suite, `T-6` focused failures that cannot be attributed.
- **Closeout phase (after every task card is done)** — the full three-gate run above is **mandatory**, and every bug it exposes must be fixed: forward-fix (already-pushed commits and published artifacts are never rolled back), then **re-run the full suite to green**. A failure judged pre-existing needs reproduction evidence on the plan's baseline commit plus a `FIX-*` / `DEFER-*` entry — "it was already red" is not an accepted disposition.

Tradeoff to be aware of: `.github/workflows/base.yml` runs on `pull_request` only, so a direct push to `main` has no remote full-suite safety net; the closeout gate is the only backstop.

Changes made outside a plan (one-off fixes, ad-hoc work) still run all three gates before being reported complete.

## Commit & PR Conventions

### Commit Messages

Use typed summaries with optional scope:
```
feat(status): support porcelain v2 (#82)
fix(push): record tracking reflog (#81)
refactor(ai): extract completion trait
test(merge): add three-way merge coverage
docs(readme): update provider table
```

### PR Requirements

- All CI checks pass (format, clippy zero-warnings, tests)
- State intent, linked issues, and tests run
- Include repro steps or sample CLI output for user-visible changes
- Keep changes small and cohesive
- Update README/CLI docs when adding flags or altering behavior
- Version bumps must update all three version faces together — `Cargo.toml`, `install.sh` `DEFAULT_VERSION`, `install.ps1` `$DefaultVersion` — or `compat_version_surface_sync` fails; never hand-edit `Cargo.lock`. Releases are cut by pushing a `v<version>` tag (`release.yml`)
- Commit with `libra commit -s` (DCO `Signed-off-by` trailer). GPG signing comes from the repository vault key by default (`vault.signing` / `commit.gpgSign`); `libra commit` exposes no `-S`, only `--no-gpg-sign`

## Database Schema

SQLite database at `.libra/libra.db` — inspect the concrete table set in the bootstrap SQL below (Git core, AI threads/scheduling, and AI runtime-contract groups).

Bootstrap files: `sql/sqlite_20260309_init.sql` (core + AI baseline) and `sql/sqlite_20260415_ai_runtime_contract.sql` (runtime-contract extension).

**Versioned migrations** live under `sql/migrations/` and are applied by `internal::db::migration::MigrationRunner`. Filenames follow `YYYYMMDDNN_<snake_case_name>.sql` (forward) with optional matching `*_down.sql` (rollback). Forward DDL should be idempotent (`CREATE TABLE IF NOT EXISTS …`); RENAME-based rebuilds are the exception — the runner's claim-first transaction guarantees single application. See `sql/migrations/README.md`.

## Environment Variables

### AI Providers
The internal LLM provider stack was removed with the Code runtime in 0.23.x, so no provider `*_API_KEY` / `*_BASE_URL` variable is read anymore; `libra review` / `libra investigate` drive external agent CLIs instead. The only remaining readers are secret-**redaction** patterns in `src/internal/ai/hardening.rs` / `src/internal/ai/observed_agents/redaction.rs`, which recognize names such as `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `MOONSHOT_API_KEY` only to scrub them from captured agent data.

### Cloud Storage (S3/R2)
`LIBRA_STORAGE_TYPE`, `LIBRA_STORAGE_BUCKET`, `LIBRA_STORAGE_ENDPOINT`, `LIBRA_STORAGE_REGION`, `LIBRA_STORAGE_ACCESS_KEY`, `LIBRA_STORAGE_SECRET_KEY`, `LIBRA_STORAGE_THRESHOLD`, `LIBRA_STORAGE_CACHE_SIZE`, `LIBRA_STORAGE_ALLOW_HTTP` (set to `"true"` to permit non-TLS HTTP endpoints, useful for local/dev S3-compatible stores). Inspect the resolved tier/threshold/cache-budget with `libra cache info` (`--json`)

### Cloud Backup (D1/R2)
`LIBRA_D1_ACCOUNT_ID`, `LIBRA_D1_API_TOKEN`, `LIBRA_D1_DATABASE_ID`; `LIBRA_D1_API_BASE_URL` overrides the Cloudflare D1 API base URL for cloud `clone`

### Build & Runtime
- `LIBRA_LOG`, `RUST_LOG` — `tracing-subscriber` env filter
- `LIBRA_LOG_FILE` — tracing sink path (append-mode by default; time-rolled when `LIBRA_LOG_ROTATION` is set)
- `LIBRA_LOG_ROTATION` — rolling strategy for `LIBRA_LOG_FILE`: `never` (default) / `minutely` / `hourly` / `daily` (`tracing-appender`, time-split only — no old-file pruning); inspect via `libra logfile info`
- `LIBRA_SYNC_DATA` — set to `1`/`true`/`yes`/`on` to fsync local object writes for power-loss durability (same as the global `--sync-data` flag)
- `LIBRA_READ_POLICY` — tiered-storage object read source: `auto` (default, local-first then remote) / `offline` / `local` (local-only) / `remote` (refresh from durable tier). An unrecognized value is a hard error (a typo must not silently re-enable remote reads). The global `--offline` flag overrides this to local-only. No-op for local-only repos
- `LIBRA_MAX_CONNECTIONS` — max concurrent remote connections/requests (positive integer; default 16), bounding remote fan-out (e.g. `exist_batch`) on large repos/CI. The global `--max-connections` flag overrides it; an invalid value is a hard error. No-op for local-only operations
- `LIBRA_FETCH_CONNECT_TIMEOUT_MS`, `LIBRA_FETCH_IDLE_TIMEOUT_MS`, `LIBRA_FETCH_FIRST_BYTE_TIMEOUT_MS` — remote fetch timeouts in milliseconds; take precedence over the `fetch.<remote>.<key>` / `fetch.<key>` config values (`connectTimeout` / `idleTimeout` / `firstByteTimeout`, whole seconds); an unparseable or `0` value falls through to the next source; the first-byte timeout applies to the `git://` transport only
- `LIBRA_PAGER` — paging policy: `always` / `never` (any other value, or unset, means `auto`); the pager command is always `less -R -F` — the system `PAGER` variable is not consulted
- `LIBRA_NO_HIDE_PASSWORD` — show password prompts in plain text (debugging)
- `LIBRA_HOME` — per-user Libra state directory (installer default `~/.libra`; any non-empty value is accepted, but use an absolute path — the upgrade install-directory lock rejects relative paths; holds `upgrade/settings.json` and upgrade state)
- `LIBRA_CONFIG_GLOBAL_DB` / `LIBRA_CONFIG_SYSTEM_DB` — override the global (`~/.libra/config.db`) / system (`/etc/libra/config.db`) config SQLite path (`LIBRA_CONFIG_GLOBAL_DB` also redirects upgrade state for test isolation)
- `LIBRA_COMMITTER_NAME` / `LIBRA_COMMITTER_EMAIL` — committer identity overrides (consulted after `GIT_COMMITTER_NAME` / `GIT_AUTHOR_NAME` and `GIT_COMMITTER_EMAIL` / `GIT_AUTHOR_EMAIL` / `EMAIL`)
- `LIBRA_SSH_COMMAND` — ssh binary used for the SSH transport (default `ssh`); `LIBRA_SSH_STRICT_HOST_KEY_CHECKING` (or `ssh.strictHostKeyChecking`) accepts `ask` / `yes` / `accept-new` / `no`. `ask` omits that SSH option so `~/.ssh/config` governs the host-key policy. Libra always passes `BatchMode=yes`, including with terminal stdin: load or unlock encrypted keys in `ssh-agent`, and verify host fingerprints through a trusted channel before updating `known_hosts` or accepting a separate interactive SSH connection. A changed-key warning remains distinct and requires verification before replacing an existing entry. SSH stderr is captured with bounded retention; see `docs/commands/config.md`.
- `LIBRA_NO_HOOKS` — set to `1`/`true`/`yes`/`on` to skip repository hooks under `.libra/hooks`; hook processes receive `LIBRA_HOOK_NAME`, `LIBRA_DIR`, `LIBRA_COMMON_DIR`, `LIBRA_HOOK_SOURCE`, `LIBRA_WORK_TREE` (`docs/commands/repository-hooks.md`)
- `LIBRA_CODE_LEASE_DURATION_MS`, `LIBRA_CODE_SESSION_WRITE_RATE_LIMIT` / `LIBRA_CODE_SESSION_WRITE_RATE_WINDOW_SECS` — removed with `libra code` in 0.23.0 (historical; ignored)
- `LIBRA_SANDBOX_ENFORCEMENT`, `LIBRA_SANDBOX_NETWORK_DISABLED`, `LIBRA_LINUX_SANDBOX_EXE`, `LIBRA_USE_LINUX_SANDBOX_BWRAP`, `LIBRA_BWRAP_BINARY` (absolute path override for `bwrap`), `LIBRA_SECCOMP_POLICY` (path to a precompiled seccomp BPF policy; unset → `~/.libra/seccomp.bpf`, auto-compiled from the bundled template on Linux when missing; an empty value disables seccomp — `docs/sandbox-seccomp.md`) — sandbox toggles (`docs/development/commands/sandbox.md`, `docs/development/tracing/sandbox.md`)
- `LIBRA_ERROR_JSON`, `LIBRA_FINE_EXIT_CODES` — stable-error-code surface toggles
- `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` / `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SDK_DISABLED` — OTLP telemetry gate (only with `--features otlp`; no default endpoint — unset means nothing is exported)

The following are baked-in constants (no env-var override) — listed
here so contributors do not waste time trying to set them at runtime:

- `LIBRA_ISSUES_URL` (`src/utils/error.rs:59`) — canonical GitHub
  issues URL appended to internal-invariant error hints.

### Tests
- `LIBRA_TEST_GITHUB_TOKEN`, `LIBRA_TEST_GITHUB_NAMESPACE` — L2 GitHub gate (creates/deletes a temporary `libra-test-*` repo); the `clone_test` GitHub scenarios additionally require `LIBRA_TEST_GITHUB_LIVE=1`
- Live S3/R2 storage tests (`cloud_storage_backup_test`) reuse the `LIBRA_STORAGE_*` / `LIBRA_D1_*` variables above and require `--features test-live-cloud` (`storage_r2_test` is L1 in-memory and needs no env vars); no separate `LIBRA_TEST_S3_*` variables are read by the suite (the names in `.env.test.example` are legacy)
- `LIBRA_TEST_MEGA_SERVER` — LFS protocol live-server gate; `MEGA_FASTCDC_READY_FILE` — connection file for the ignored `mega_fastcdc_http_interop` test (`--features fastcdc`); `LIBRA_BACKEND_CHECKOUT` — sibling `libra-backend` checkout for `upgrade_publish_contract_test` (default `../libra-backend`)
- `LIBRA_RUN_LIVE_AGENT_GATE=1` — `test-live-agent` gate (`agent_live_gate_test`; real local `claude` / `codex` / `opencode` stores); `LIBRA_RUN_LOCAL_AGENTS=1` — Wave 7 `agent_local_capture_smoke_test` (drives real local agent sessions; tuned by `LIBRA_LOCAL_AGENT_SET`)
- `LIBRA_TEST_HOME` — test-only home-directory override; `LIBRA_TEST_LOG=1` (or `RUST_LOG`) — opt-in tracing output from the shared test helpers in `src/utils/test.rs` (quiet by default); `LIBRA_TEST=1` — test sentinel read by the binary (pager, maintenance lock, operation wrapper, `am` failpoints, debug-only `stash` / `status` seams, `test-upgrade` trust-root injection)

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
