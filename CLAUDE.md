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
libra commit -m "..."     # not: git commit
libra log                 # not: git log
libra diff                # not: git diff
libra branch / switch / checkout / merge / rebase / push / pull / fetch …
```

Compatibility is *partial* and governed by the four-tier matrix in [`COMPATIBILITY.md`](COMPATIBILITY.md) (`supported` / `partial` / `unsupported` / `intentionally-different`) — consult it before assuming a Git flag or subcommand behaves identically. Libra also adds AI-native subcommands with no Git equivalent (`code`, `code-control`, `automation`, `usage`, `graph`, `sandbox`, `agent`, `publish`).

(Note: this constraint is about operating *in* this repo. To build/test the Libra source itself, use the `cargo` commands in **Build & Development Commands** below; to run the in-tree build of the CLI use `cargo run -- <command>`.)

## Project Overview

Libra is an **AI agent–native version control system** written in Rust. It partially implements a Git client with full on-disk format compatibility (`objects`, `index`, `pack`, `pack-index`) while using SQLite for transactional metadata (`config`, `HEAD`, `refs`). It is designed for monorepo/trunk-based development with tiered cloud storage (S3/R2) and a Cloudflare D1/R2 backup path.

The `libra code` command launches an interactive TUI (with a background web server, MCP server, and an automation-control session surface) for collaborative AI-agent and human-driven development. The Git surface is governed by a four-tier compatibility matrix (`supported` / `partial` / `unsupported` / `intentionally-different`) tracked in [`COMPATIBILITY.md`](COMPATIBILITY.md); AI-only commands (`code`, `code-control`, `automation`, `usage`, `graph`, `sandbox`, `agent`, `publish`) are explicitly Libra-only extensions.

The repository also contains a Next.js frontend (`web/`) embedded into the binary via `rust-embed` and a Cloudflare Worker (`worker/`) for read-only `libra publish` hosting.

## Build & Development Commands

### Essential Commands

```bash
# Format code (requires nightly toolchain)
cargo +nightly fmt --all

# Lint — all warnings must be resolved before committing (all features on)
cargo clippy --all-targets --all-features -- -D warnings

# Quick compile check (skip the Next.js web build for speed)
LIBRA_SKIP_WEB_BUILD=1 cargo check
LIBRA_SKIP_WEB_BUILD=1 cargo build

# Run full test suite (L1 only by default; L2/L3 auto-skip when env vars are unset)
cargo test --all

# Run specific tests
cargo test command::init_test
cargo test add_test

# Run the CLI
cargo run -- <command>          # e.g. cargo run -- status

# Build the embedded web frontend (normally driven by build.rs)
pnpm --dir web install --frozen-lockfile && pnpm --dir web build
```

### Cargo Features

| Feature | Purpose |
|---------|---------|
| `worktree-fuse` | Enable Unix FUSE-backed worktree commands (Linux/macOS only) |
| `test-network` | Gate L2 tests requiring outbound network but no secrets |
| `test-live-ai` | Gate L3 tests calling real LLM APIs |
| `test-live-cloud` | Gate L3 tests hitting real D1/R2 endpoints |
| `test-provider` | Deterministic hidden provider for local TUI automation tests (requires `LIBRA_ENABLE_TEST_PROVIDER=1`) |
| `test-live-agent` | plan-20260713 live agent gate: real local `claude`/`codex`/`opencode` CLI data on the dev acceptance machine (requires `LIBRA_RUN_LIVE_AGENT_GATE=1`; missing stores print skipped) |
| `subagent-scaffold` | Schema-only sub-agent contract scaffold (CEX-S2-10, gated on CP-4 in production) |
| `otlp` | OTLP trace export (lore.md 1.7): one vetted command-span to an explicitly configured collector; default binary unaffected |

### CI Pipeline (`.github/workflows/base.yml`)

All PRs must pass these jobs on the `[self-hosted]` runner pool:
1. **compat-rustfmt** — `cargo +nightly fmt --all --check`
2. **compat-clippy** — `cargo clippy --all-targets --all-features -- -D warnings` (with `LIBRA_SKIP_WEB_BUILD=1`)
3. **compat-web-check** — `pnpm --dir web lint` + `pnpm --dir web build` so `web/out/` cannot drift from `WebAssets`
4. **compat-redundancy** — directory-shape check on `third-party/rust/crates`
5. **compat-offline-core** — `cargo test --test compat_matrix_alignment compatibility_matrix_matches_cli_commands -- --exact` + `cargo test --all` + a second pass with `--features test-provider` for the TUI automation matrices (`code_ui_scenarios`, `harness_self_test`, `code_codex_default_tui_test`, `code_ui_remote_lease_matrix`, `code_ui_remote_sse_matrix`) under `--test-threads=1`
6. **compat-network-remotes** — `cargo test --features test-network --test network_remotes_test`

Additional workflows: `codeql.yml` (security analysis), `model-generation-nightly.yml` (nightly model-generation matrix), `release.yml` (release pipeline).

## Test Layers

Libra tests are organised into three layers — `cargo test --all` runs L1 only; L2/L3 are silently skipped when their env vars are unset. See `docs/tests.md` for the canonical guide.

| Layer | Dependencies | Trigger |
|-------|--------------|---------|
| **L1 — Deterministic** | None (tempdir, in-memory stores, mock models) | `cargo test --all` |
| **L2 — Network** | GitHub token for temporary repo creation | `LIBRA_TEST_GITHUB_TOKEN` + `LIBRA_TEST_GITHUB_NAMESPACE` |
| **L3 — Live Services** | Real AI API keys (`DEEPSEEK_API_KEY`, `MOONSHOT_API_KEY`, …) or cloud credentials (`LIBRA_D1_*`, `LIBRA_STORAGE_*`, `LIBRA_TEST_S3_*`) | Set the relevant env vars |

Gate L2 / L3 tests with the small `env_var_is_set(name) -> bool` helper (see e.g. [`tests/cloud_storage_backup_test.rs:30`](tests/cloud_storage_backup_test.rs)) followed by an early `eprintln!("skipped (...)")` return when a required var is unset — missing vars print "skipped", never fail. Copy `.env.test.example` → `.env.test` and `source` it before running the full suite (the `export` prefix is required).

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
- **Compatibility-surface tests** in `tests/compat/` guard against regressions in CLI flag/help wording, declined-feature drift, and the production `unwrap()` audit. Each `*.rs` under `tests/compat/` must be registered as a `[[test]]` entry in `Cargo.toml` (Cargo's default discovery only picks up files directly under `tests/`). New compat guards must also add a row to the inventory table in [`tests/compat/README.md`](tests/compat/README.md). See [`docs/tests.md`](docs/tests.md) `Compatibility-surface tests` section for the full convention.
- **Cross-cutting `--help` EXAMPLES contract**: every visible command in `src/cli.rs::Commands` ships with a `pub const <CMD>_EXAMPLES` constant wired via `#[command(after_help = …)]` (or `after_help = command::<name>::<CMD>_EXAMPLES` on the parent subcommand binding in `cli.rs` for `Subcommand`-style commands). Three compat guards protect this contract: `compat_help_examples_banner` (every `<cmd> --help` renders an EXAMPLES section), `cli::tests::root_after_help_lists_every_visible_command` (every non-hidden command appears in a Command Groups row), and `compat_command_docs_examples_section` (every `docs/commands/<name>.md` page carries an Examples / Common Commands heading). New commands must satisfy all three.
- **Isolation**: Use `tempfile::tempdir()` and `utils::test::ChangeDirGuard` to isolate state
- **Serial execution**: Mark tests `#[serial]` (from `serial_test` crate) if they mutate shared state
- **Async tests**: Use `#[tokio::test]` (or `flavor = "multi_thread"` when needed)
- **Fixtures**: Keep small and local in `tests/data/` and `tests/fixtures/`; reuse helpers from `tests/command/mod.rs`, `tests/harness/`, and `tests/helpers/`
- **Gating**: Use the `env_var_is_set(name)` helper pattern (see `tests/cloud_storage_backup_test.rs:30`) plus an early `eprintln!("skipped (set ...)")` return so missing vars print a skip notice and do not fail the test. Match the L1/L2/L3 layering and the matching `test-network` / `test-live-ai` / `test-live-cloud` Cargo features
- **Coverage**: Pair new commands/options with at least one end-to-end test plus a focused unit test, and an entry in `COMPATIBILITY.md` if you change the Git surface. New `StableErrorCode` variants must also be added to `docs/error-codes.md` (the `compat_error_codes_doc_sync` guard fails the build otherwise).

## Quality Acceptance Criteria (质量验收标准)

A change is considered done only when all three of the following pass locally with no manual fix-ups:

1. **Formatting** — `cargo +nightly fmt --all --check` reports no formatting differences.
2. **Lint** — `cargo clippy --all-targets --all-features -- -D warnings` reports no warnings.
3. **Tests** — `source .env.test && cargo test --all` passes in full (L1 always runs; L2/L3 print "skipped" rather than fail when their env vars are unset — that is acceptable, an actual failure is not).

These mirror the `compat-rustfmt`, `compat-clippy`, and `compat-offline-core` CI jobs, so passing them locally is the precondition for opening a PR. Run all three before reporting work as complete.

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

## Database Schema

SQLite database at `.libra/libra.db` — inspect the concrete table set in the bootstrap SQL below (Git core, AI threads/scheduling, and AI runtime-contract groups).

Bootstrap files: `sql/sqlite_20260309_init.sql` (core + AI baseline) and `sql/sqlite_20260415_ai_runtime_contract.sql` (runtime-contract extension).

**Versioned migrations** live under `sql/migrations/` and are applied by `internal::db::migration::MigrationRunner`. Filenames follow `YYYYMMDDNN_<snake_case_name>.sql` (forward) with optional matching `*_down.sql` (rollback). Forward DDL must be idempotent (`CREATE TABLE IF NOT EXISTS …`). See `sql/migrations/README.md`.

The publish Worker uses its own D1 schema in `sql/publish/` (`0001_publish.sql`, `0002_publish_digest_check.sql`, `0003_publish_max_preview_trigger_replace.sql`, `0004_publish_refs_index.sql`).

## Environment Variables

### AI Providers
| Provider | API Key Env | Base URL Override |
|----------|-------------|-------------------|
| `gemini` | `GEMINI_API_KEY` | — |
| `openai` | `OPENAI_API_KEY` | `OPENAI_BASE_URL` |
| `anthropic` | `ANTHROPIC_API_KEY` | `ANTHROPIC_BASE_URL` |
| `deepseek` | `DEEPSEEK_API_KEY` | `--api-base` only (no env var) |
| `kimi` | `MOONSHOT_API_KEY` | `MOONSHOT_BASE_URL` |
| `zhipu` | `ZHIPU_API_KEY` | `ZHIPU_BASE_URL` |
| `ollama` | — | `OLLAMA_BASE_URL` or `--api-base` |

### Cloud Storage (S3/R2)
`LIBRA_STORAGE_TYPE`, `LIBRA_STORAGE_BUCKET`, `LIBRA_STORAGE_ENDPOINT`, `LIBRA_STORAGE_REGION`, `LIBRA_STORAGE_ACCESS_KEY`, `LIBRA_STORAGE_SECRET_KEY`, `LIBRA_STORAGE_THRESHOLD`, `LIBRA_STORAGE_CACHE_SIZE`, `LIBRA_STORAGE_ALLOW_HTTP` (set to `"true"` to permit non-TLS HTTP endpoints, useful for local/dev S3-compatible stores). Inspect the resolved tier/threshold/cache-budget with `libra cache info` (`--json`)

### Cloud Backup (D1/R2)
`LIBRA_D1_ACCOUNT_ID`, `LIBRA_D1_API_TOKEN`, `LIBRA_D1_DATABASE_ID`

### Build & Runtime
- `LIBRA_SKIP_WEB_BUILD=1` — skip the Next.js web build in `build.rs` (set by every CI job except `compat-web-check`)
- `LIBRA_LOG`, `RUST_LOG` — `tracing-subscriber` env filter
- `LIBRA_LOG_FILE` — tracing sink path (append-mode by default; time-rolled when `LIBRA_LOG_ROTATION` is set)
- `LIBRA_LOG_ROTATION` — rolling strategy for `LIBRA_LOG_FILE`: `never` (default) / `minutely` / `hourly` / `daily` (`tracing-appender`, time-split only — no old-file pruning); inspect via `libra logfile info`
- `LIBRA_SYNC_DATA` — set to `1`/`true`/`yes`/`on` to fsync local object writes for power-loss durability (same as the global `--sync-data` flag)
- `LIBRA_READ_POLICY` — tiered-storage object read source: `auto` (default, local-first then remote) / `offline` / `local` (local-only) / `remote` (refresh from durable tier). An unrecognized value is a hard error (a typo must not silently re-enable remote reads). The global `--offline` flag overrides this to local-only. No-op for local-only repos
- `LIBRA_MAX_CONNECTIONS` — max concurrent remote connections/requests (positive integer; default 16), bounding remote fan-out (e.g. `exist_batch`) on large repos/CI. The global `--max-connections` flag overrides it; an invalid value is a hard error. No-op for local-only operations
- `LIBRA_PAGER` — pager override (falls back to system `PAGER` then `less`)
- `LIBRA_NO_HIDE_PASSWORD` — show password prompts in plain text (debugging)
- `LIBRA_CONFIG_GLOBAL_DB` — override the global config SQLite path
- `LIBRA_COMMITTER_NAME` / `LIBRA_COMMITTER_EMAIL` — committer identity overrides
- `LIBRA_SSH_COMMAND`, `LIBRA_SSH_STRICT_HOST_KEY_CHECKING` — SSH protocol tuning
- `LIBRA_CODE_LEASE_DURATION_MS` — `libra code` automation lease length
- `LIBRA_SANDBOX_ENFORCEMENT`, `LIBRA_SANDBOX_NETWORK_DISABLED`, `LIBRA_LINUX_SANDBOX_EXE`, `LIBRA_USE_LINUX_SANDBOX_BWRAP` — sandbox toggles (`docs/development/commands/sandbox.md`)
- `LIBRA_ERROR_JSON`, `LIBRA_FINE_EXIT_CODES` — stable-error-code surface toggles
- `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` / `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SDK_DISABLED` — OTLP telemetry gate (only with `--features otlp`; no default endpoint — unset means nothing is exported)

The following are baked-in constants (no env-var override) — listed
here so contributors do not waste time trying to set them at runtime:

- `LIBRA_VCS_TIMEOUT_SECONDS` (`src/internal/ai/mcp/resource.rs:86`) —
  MCP-side AI-VCS tool timeout, currently fixed at 120 s.
- `LIBRA_VCS_DEFAULT_APPROVAL_SCOPE` (`src/internal/ai/sources/mcp.rs:28`)
  — default approval scope for `run_libra_vcs`, currently `interactive`.
- `LIBRA_ISSUES_URL` (`src/utils/error.rs:59`) — canonical GitHub
  issues URL appended to internal-invariant error hints.

### Tests
- `LIBRA_TEST_GITHUB_TOKEN`, `LIBRA_TEST_GITHUB_NAMESPACE` — L2 GitHub gate (creates/deletes a temporary `libra-test-*` repo)
- `LIBRA_TEST_S3_ENDPOINT`, `LIBRA_TEST_S3_BUCKET`, `LIBRA_TEST_S3_ACCESS_KEY`, `LIBRA_TEST_S3_SECRET_KEY`, `LIBRA_TEST_S3_REGION`, `LIBRA_TEST_S3_ALLOW_HTTP` — L3 S3 protocol gate (separate from the R2 backup credentials above)
- `LIBRA_PUBLISH_LIVE_WORKER_ORIGIN`, `LIBRA_PUBLISH_LIVE_CLONE_DOMAIN`, `LIBRA_PUBLISH_LIVE_SLUG`, `LIBRA_PUBLISH_LIVE_FILE_PATH` — `publish_live` deploy-smoke gate
- `LIBRA_TEST_MEGA_SERVER` — LFS protocol live-server gate
- `LIBRA_ENABLE_TEST_PROVIDER` — activate the `test-provider` deterministic LLM for TUI scenarios (required alongside `--features test-provider`)
- `LIBRA_TEST_LOG`, `LIBRA_TEST_HOME`, `LIBRA_TEST_ENV` — test-only logging/home/sentinel overrides
