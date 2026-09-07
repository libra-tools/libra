# AGENTS.md

## Review guidelines

Review pull requests with a high-recall, production-risk mindset.

Prioritize finding issues that could plausibly cause:
- security exposure
- incorrect behavior
- silent failure
- data corruption or data loss
- backward incompatibility
- production instability
- missing validation or missing tests around changed behavior
- incomplete documentation for externally visible changes

Do not optimize for fewer comments.
If an issue is plausible, user-impacting, and actionable, raise it.

Focus areas:

- Code quality: readability, maintainability, error handling, edge cases, control flow clarity, and failure modes.
- Security: vulnerabilities, unsafe defaults, input validation/sanitization, secrets exposure, auth/authz regressions, injection risks, SSRF/path traversal/deserialization/crypto misuse where relevant.
- Performance: hot-path regressions, N+1 queries, unbounded work, excessive allocations, blocking I/O, memory/resource leaks, missing pagination/batching/caching where impact is likely.
- Testing: missing coverage for changed behavior, weak assertions, missing regression tests, missing edge-case coverage, flaky test risk.
- Documentation: missing or stale code comments, README updates, migration notes, config/env var docs, API/SDK/schema docs, changelog or release notes for externally visible behavior.

## Review output

- Use inline comments for specific issues tied to changed files.
- Use a top-level summary for cross-cutting risks, overall assessment, and praise.
- Order findings by severity, then user impact, then ease of verification.
- For each finding, explain:
  - what is wrong
  - why it matters
  - the realistic impact
  - the minimal fix or direction
- Prefer one clear finding per issue. Avoid combining unrelated concerns.
- Avoid style-only nitpicks unless they create maintainability or correctness risk.
- If no material issues are found, say so briefly and note any residual risk or untested area.

## Severity policy

Treat the following as P0/P1 when applicable:

### P0
- vulnerabilities enabling unauthorized access, data exfiltration, remote code execution, privilege escalation, or destructive data loss
- changes likely to cause major outage, irreversible corruption, or widespread security exposure

### P1
- missing or weakened authentication, authorization, tenancy, or permission checks
- untrusted input reaching dangerous sinks without adequate validation or sanitization
- silent exception swallowing, hidden failure paths, or incorrect fallback behavior
- correctness bugs in business logic, state transitions, retries, idempotency, concurrency, or transaction handling
- backward-incompatible API, schema, contract, or migration changes without explicit handling
- performance regressions likely to affect latency, throughput, reliability, or infrastructure cost in production
- missing tests for changed behavior, bug fixes, edge cases, or critical failure paths
- missing README, migration, configuration, or API documentation for externally visible changes
- unsafe logging of secrets, tokens, PII, or sensitive internal details
- resource lifecycle issues: leaked handles, unreleased locks, missing timeouts, unbounded retries, or unbounded memory growth
- use of `unwrap()` or `expect()` in production code (library/command modules, including startup/initialization) — must be replaced with proper error handling (`?`, `anyhow::Context`, `thiserror`) that produces user-friendly error messages. Acceptable only in tests and obviously infallible logic with a `// INVARIANT:` comment; flag all other occurrences

## Trigger rules

When deciding whether to raise a finding, err toward reporting if:
- the change affects auth, permissions, payments, data writes, migrations, caching, concurrency, retries, or external APIs
- the PR changes public behavior but tests or docs were not updated
- error handling changed and failure behavior is not explicitly tested
- a query, loop, or network call is added in a potentially hot path
- defaults changed in a way that could affect security or production behavior
- a fix depends on assumptions not enforced in code
- any new or modified code introduces `unwrap()`, `expect()`, or `panic!()` outside of tests or obviously infallible logic — flag the instance and suggest a `Result`-based alternative with a contextual, user-friendly error message

Do not dismiss an issue only because:
- the diff is small
- the code “probably works”
- the risk depends on a realistic edge case
- the fix would be easy to add later

## Commenting style

- Inline comments: concrete file-specific defects or risks.
- Top-level summary: overall risk, recurring themes, and praise.
- Be direct and specific.
- Prefer actionable recommendations over generic advice.

## Project Structure & Module Organization
- `src/` holds the Rust crate (edition 2024). CLI entry `src/main.rs`, library root `src/lib.rs`, CLI definition/dispatch in `src/cli.rs`, shared helpers in `src/common_utils.rs`, `src/git_protocol.rs`, and `src/lfs_structs.rs`.
- `src/command/` contains every `libra <subcommand>` (Git-compatible commands plus Libra-specific commands such as `code`, `automation`, `usage`, `graph`, `sandbox`, `review`, `investigate`, `cloud`, `publish`, `upgrade`, `op`, `hydrate`, `sparse-view`, `layer`, `deps`, `alternates`, `metadata`, `media` (only with `--features fastcdc`), and `agent/*`). `code-control` and `db` are no longer commands (`code-control` was removed in W5-01; use `libra code --control stdio`); `src/command/package.rs` (CEX-S2-17 `libra package`) exists but is not yet wired into `src/cli.rs` — whether it enters the stable command surface is still an open decision (`docs/development/internal/package.md`).
- `src/internal/` holds core logic: AI stack in `internal/ai/` (`agent/`, `agent_run/`, `agent_bridge/`, `observed_agents/`, `providers/`, `tools/`, `completion/`, `mcp/`, `session/`, `prompt/`, `commands/`, `hooks/`, `intentspec/`, `orchestrator/`, `goal/`, `skills/`, `sandbox/`, `permission/`, `runtime/`, `usage/`, `web/`, `automation/`, `codex/`, `projection/`, `review/`, `investigate/`, `capability_package/`, `context_budget/`, `sources/`, `history.rs`, `libra_vcs.rs`, …); Sea-ORM models in `internal/model/`; network clients in `internal/protocol/` (`git_client`, `https_client`, `ssh_client`, `lfs_client`, `local_client`); publish pipeline in `internal/publish/`; other core modules include `internal/db/` + `internal/db.rs` (migrations), `internal/sequencer/`, `internal/sparse/`, `internal/layer/`, `internal/deps/`, `internal/alternates/`, `internal/obliteration/`, `internal/upgrade/` (signed self-upgrade), `internal/worktree_io/`, `internal/operation.rs` (operation log v1), and `internal/workspace.rs` (workspace leases).
- `src/utils/` covers shared utilities: `client_storage.rs` (tiered local + S3/R2 + LRU), `d1_client.rs`, path/object/tree helpers, `ignore.rs`, `lfs.rs`, `fuse.rs`, `convert.rs`, `error.rs`, `output.rs`, `pager.rs`, `text.rs`, `worktree.rs`, `storage/`, `storage_ext.rs`, `media/` (FastCDC), `pathspec/`, `read_policy.rs`, `redact.rs`, `telemetry.rs`, and `test.rs` (`ChangeDirGuard`, `setup_with_new_libra_in`).
- `tests/` holds integration targets at the top level plus `tests/command/` for per-subcommand suites (all `tests/command/*.rs` compile into the single `command_test` binary). `tests/INDEX.md` is the authoritative one-line index of every cargo `--test` target, grouped by Wave (1 command/compat, 1F feature-gated deterministic, 2 Code UI & local automation, 3 network, 4 live AI, 5 live cloud, 6 perf smoke, 7 local agent capture smoke); keep it in sync when adding/renaming a test target. Shared helpers live in `tests/command/mod.rs`, `tests/helpers/`, and `tests/harness/`; fixtures live in `tests/data/` and `tests/fixtures/`; `tests/compat-ledger/` is the Git-compatibility evidence ledger (one row per migrated upstream Git scenario, schema-guarded by `compat_ledger_schema`); `tests/compat/` covers cross-command compatibility guards; `tests/SERIAL_REGISTRY.tsv` + `tests/NEXTEST_GROUPS.sh` generate `.config/nextest.toml`.
- `web/` is the Next.js static export generated by `build.rs` (skipped when `LIBRA_SKIP_WEB_BUILD=1` is set) and embedded into `WebAssets` by `src/command/web_assets.rs`. `worker/` holds the read-only Cloudflare Worker (Next.js + OpenNext, D1 + R2) that serves `libra publish` snapshots.
- Community docs live in `docs/` (including `docs/development/integration/integration-test-plan.md`, the command development notes under `docs/development/commands/`, implementation targets under `docs/development/tracing/`, competitor-gap plans under `docs/development/gap/`, internal designs under `docs/development/internal/`, and dated task-card plans under `docs/development/plan/`); SQLite bootstrap lives in `sql/sqlite_20260309_init.sql` plus `sql/sqlite_20260415_ai_runtime_contract.sql`; runtime migrations live in `sql/migrations/`; publish-pipeline schema lives in `sql/publish/`; hooks/templates live in `template/`; release/install assets live in `install.sh` and `install.ps1` (both are version faces guarded by `compat_version_surface_sync`); `benchmark/` holds the reproducible CLI performance benchmark.

## Build, Test, and Development Commands
- `cargo +nightly fmt --all` then `cargo clippy --all-targets --all-features -- -D warnings` keep formatting and linting aligned (`rustfmt.toml` sets `group_imports = "StdExternalCrate"` and `imports_granularity = "Crate"`). **CI enforces `-D warnings`; all clippy warnings must be resolved before committing.**
- `cargo build` or `cargo check` for quick compile checks; `cargo run -- <cmd>` exercises the CLI (for example, `cargo run -- status` in a temp repo). Set `LIBRA_SKIP_WEB_BUILD=1` to skip the Next.js export inside `build.rs` during iteration.
- `cargo test --all` runs the default L1 suite (the acceptance gate). Filter with `cargo test --test command_test <module>` (e.g. `add_test`) or any other top-level `--test <target>` from `tests/INDEX.md`. CI runs the same suite as `cargo nextest run --all --no-fail-fast --retries 2` (pinned `cargo-nextest 0.9.143`; groups from the generated `.config/nextest.toml`). Integration cases rely on temp dirs; mark `#[serial(<lane>)]` if they mutate shared state.
- Feature-gated layers (see `tests/INDEX.md` waves; CI job `compat-offline-core` runs the default-feature suite with L2/L3 secrets injected — step "Run tests (L1 + L2 + L3)"; `required-features` targets are excluded — plus dedicated `otlp` / `keyring` / `test-upgrade` steps; `compat-network-remotes` runs L2):
  - `--features test-network` for Wave 3 (`network_remotes_test`) — no secrets needed.
  - `--features test-live-ai` for Wave 4 (real LLM APIs; needs `DEEPSEEK_API_KEY` etc.; `ai_agent_test`, `ai_chat_agent_test` and `ai_ollama_live_gate_test` are `required-features`-gated, `code_ui_remote_model_generation_matrix` is `#[ignore]`-gated).
  - `--features test-live-cloud` for Wave 5 (real D1/R2; needs `LIBRA_D1_*`/`LIBRA_STORAGE_*`, plus `LIBRA_ENABLE_TEST_LIVE_CLOUD=1` for `publish_live_test`).
  - `--features test-provider` plus `LIBRA_ENABLE_TEST_PROVIDER=1` to activate the deterministic provider used by `code_ui_scenarios`, `harness_self_test`, `ai_code_ui_headless_test`, `code_codex_runtime_test` and the `code_ui_remote_{lease,sse,state,security,generation,approval}_matrix` targets (`code_codex_default_web_test` is a static guard that needs no provider) — local-only since CI dropped this step on 2026-08-31; run with `LIBRA_ENABLE_TEST_PROVIDER=1 cargo nextest run --features test-provider --profile test-provider --test <target>` (single-threaded profile) or `cargo test … -- --test-threads=1`.
  - `--features test-upgrade` (with `LIBRA_TEST=1`) for Wave 1F (`upgrade_auto_test`, `upgrade_publish_contract_test`); `--features otlp` (`otlp_telemetry`), `--features keyring` (`auth_keyring_backend`), `--features test-live-agent` (`agent_live_gate_test`, `LIBRA_RUN_LIVE_AGENT_GATE=1`) and `--features fastcdc` (the `media` command and `media_fastcdc_test`) are likewise compile-time gated.
  - `--features worktree-fuse` for Unix FUSE-backed worktree commands.
  - `--features subagent-scaffold` for the gated CEX-S2-10 schema scaffold (see `docs/development/tracing/agent.md`).

## Coding Style & Naming Conventions
- Rust 2024; 4-space indent; snake_case for modules/functions, PascalCase for types, SCREAMING_SNAKE for consts.
- Imports are grouped Std/External/Crate per `rustfmt.toml`; avoid wildcard imports except in tests.
- Prefer `anyhow::Result` for CLI flows and `thiserror` for library errors; keep args parsed via `clap` in `src/command/*`.
- **Avoid `unwrap()` / `expect()`** in production code (including startup/initialization). They are acceptable only in tests and obviously infallible logic (with a `// INVARIANT:` comment). All other code must return `Result` and propagate errors with `?` plus contextual, user-friendly messages.
- **User-friendly errors**: All errors surfaced to the user must be human-readable and actionable — wrap internal errors with context explaining what went wrong, which resource was affected, and how to fix it.
- Add short comments only when control flow is non-obvious (e.g., async handling, SQLite migrations).

## Testing Guidelines
- Favor integration coverage in `tests/command/` (per-subcommand suites) and the top-level `tests/*.rs` targets (AI/runtime/Code-UI/publish/etc.) to mirror real Git and agent workflows; use `tempfile::tempdir()` and `utils::test::ChangeDirGuard` to isolate state.
- `tests/INDEX.md` is the authoritative test index — when adding/renaming a `--test` target, add or update its one-line row (target | wave | one-line purpose | relevant src). Reference specific cases in PRs as `<target>::<test_fn>`.
- Keep fixtures small and local under `tests/data/` or `tests/fixtures/`; re-use helpers in `tests/command/mod.rs`, `tests/helpers/`, and `tests/harness/` instead of shelling out directly.
- Mark tests with keyed lanes (`#[serial(cwd)]`, `#[serial(env)]`, `#[serial(hash_kind)]`, or a named external key) via `serial_test` if they mutate shared state; every annotation that `tests/SERIAL_CLASSIFY.sh` judges `global`/`lane:*` needs a matching row in `tests/SERIAL_REGISTRY.tsv` (`none`-judged annotations carry no row and are deletion candidates; `compat_serial_registry` re-runs the classifier and fails on missing, dangling or mismatched rows), and `.config/nextest.toml` must be regenerated with `sh tests/NEXTEST_GROUPS.sh` after touching the registry. Keep async tests on Tokio (`#[tokio::test]`, or `flavor = "multi_thread"` when needed). Headless Web (HTTP/SSE) harness scenarios under `tests/harness/` plus the `code_ui_*` and `harness_self_test` targets require `--features test-provider` with `LIBRA_ENABLE_TEST_PROVIDER=1` and single-threaded execution (`--test-threads=1` or the nextest `test-provider` profile).
- Pair new commands/options with at least one end-to-end test plus a focused unit test where logic is easily isolated. For new error variants, add a Display-pin test (see the `test(...): pin Display for …` commits) so user-facing messages stay stable.
- When changing compatibility-sensitive help text, docs, or public CLI flags, update the relevant `docs/commands/*.md`, `COMPATIBILITY.md`, `docs/error-codes.md`, and compat tests in the same change.
- When adding or renaming a `tests/compat/*` file, also update `Cargo.toml` and `tests/compat/README.md`; files there do not run unless registered explicitly.
- **Plan-driven work runs layered tests** (`docs/development/plan/plan-template.md` ER-13): during a plan's task-card execution phase run only the tests related to that card. `cargo +nightly fmt --all --check` and `cargo clippy --all-targets --all-features -- -D warnings` still run on every card that gets pushed; `cargo test --all` runs on a card only when it hits an ER-13 trigger (cross-cutting surfaces such as `sql/**`, `src/cli.rs`, stable error codes, shared single-source-of-truth helpers, `build.rs`, CI/repo config or installers; release/aggregation cards; removals or renames of public surfaces; shared test infrastructure under `tests/harness/`, `tests/helpers/`, `tests/command/mod.rs`; an explicit request; or unattributable focused failures). Once every task card is done the full suite is mandatory: run it, fix every bug it exposes (forward-fix), and re-run to green. Work outside a plan still runs the full suite before being called done.

## Commit & Pull Request Guidelines
- History uses short, typed summaries with optional scope and PR reference, e.g., `feat(status): support porcelain v2 (#82)` or `fix(push): record tracking reflog (#81)`.
- Commits must carry the DCO trailer and be signed: `libra commit -s -m "feat(...): ..."` (`-s`/`--signoff` adds `Signed-off-by`; GPG signing is applied by the repository vault key by default via `vault.signing` / `commit.gpgSign` — `libra commit` exposes no `-S`, only `--no-gpg-sign`). This checkout has no `.git/`, so `git commit` does not apply here.
- PRs should state intent, linked issues, and tests run (`cargo +nightly fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all`, plus any relevant `--features test-*` runs); include repro steps or sample CLI output when touching user-visible behavior. If web/worker code changed, include the matching `pnpm --dir web ...` or `pnpm --dir worker ...` verification.
- Keep changes small and cohesive; update README/CLI docs when adding flags or altering compatibility tables.

## Workspace Notes
- This repository is managed as a single Rust package plus `web/` and `worker/`; this checkout is a Libra repository (`.libra/`, no `.git/`), so all version-control operations go through `libra` commands — raw `git` does not work here.
- Prefer this command flow for changes: `libra status` -> inspect diff -> `libra add` -> `libra commit -a -s -m "<scope>: ..."` -> `libra push origin <branch>`.
- When a user requests version operations or release copy steps, follow the `libra` command workflow instead of `git` commands.

## Aggressive review bias

Prefer false-positive-tolerant review over false-negative-tolerant review for:
- security-sensitive code
- data mutations
- migrations
- public API changes
- reliability-critical paths

If uncertain between “mention in summary” and “raise as finding”,
raise as a finding when the issue could plausibly reach production.

Treat undocumented assumptions as risk.
Treat untested fixes as incomplete.
Treat externally visible behavior changes without docs as P1 by default.
Flag `unwrap()` / `expect()` in production code (outside tests and obviously infallible logic) as a P1 finding — startup/initialization is a production path and is NOT exempt. Suggest replacing with `?` + contextual, user-friendly error message via `anyhow::Context` or a domain-specific `thiserror` variant. If used in an obviously infallible scope, ensure a brief `// INVARIANT:` comment explains why the value can never be `None`/`Err`.
