# Libra - Repository Custom Instructions for GitHub Copilot

## What this repo is

Libra is a Rust implementation of an AI agent-native version control system with
Git on-disk compatibility, SQLite-backed repository metadata, durable AI runtime
state, tiered local/S3/R2 object storage, and a Cloudflare-backed publish flow.

This is a single Rust package named `libra` plus two TypeScript/Next.js surfaces:

- `web/`: the static Code UI exported by Next.js and embedded into the Rust binary.
- `worker/`: the Libra publish Cloudflare Worker, backed by D1 and R2.

Do not assume the older multi-crate `engine/`, `delta/`, `transport/`, or
`storage/` layout exists. The current implementation lives primarily under
`src/`, with command handlers in `src/command/` and shared/runtime internals in
`src/internal/` and `src/utils/`.

## Repository layout

- `src/main.rs`: binary entry point.
- `src/lib.rs`: embedding API (`exec`, `exec_async`) and public re-exports.
- `src/cli.rs`: clap root grammar, global output flags, repository preflight, and
  command dispatch.
- `src/command/`: one module per `libra <subcommand>`, including Git-compatible
  commands (`init`, `clone`, `add`, `commit`, `push`, `pull`, `status`, `log`,
  `show`, `diff`, `branch`, `switch`, `checkout`, `merge`, `rebase`, `stash`,
  `worktree`, etc.) and Libra-only commands (`code`, `agent`, `automation`,
  `usage`, `graph`, `sandbox`, `review`, `investigate`, `cloud`, `publish`,
  `upgrade`, `op`, `layer`, `sparse-view`, `hydrate`, `deps`, `alternates`,
  `metadata`; `media` only with `--features fastcdc`). `code-control` and `db`
  are not commands (`code-control` was removed in W5-01 — use
  `libra code --control stdio`).
- `src/internal/ai/`: AI runtime, providers, tools, MCP, session storage,
  permissions, sandboxing, context budget, goal mode, orchestration, skills, and
  web projections.
- `src/internal/ai/web/`: Web Code UI runtime, projection, and transport adapters.
- `src/internal/model/`: Sea-ORM models.
- `src/internal/protocol/`: Git, HTTPS, SSH, LFS, and local protocol clients.
- `src/internal/publish/`: publish snapshot/export pipeline.
- `src/utils/`: object/path/storage/output/test helpers, tiered storage,
  worktree utilities, pager support, and stable CLI error types.
- `sql/`: SQLite bootstrap schemas and migrations; `sql/publish/` is the publish
  Worker schema.
- `tests/`: integration tests. `tests/INDEX.md` is the authoritative index of
  every cargo `--test` target.
- `tests/command/`: per-command integration suites and shared command helpers.
- `tests/compat/`: cross-command compatibility guards that must also be
  registered as `[[test]]` entries in `Cargo.toml`.
- `docs/commands/`: user-facing command docs, kept in sync with the CLI surface.
- `COMPATIBILITY.md`: compatibility matrix guarded by tests.
- `.github/workflows/`: CI gates. `base.yml` is the main PR gate.

## Languages and defaults

- Rust edition: 2024.
- Primary runtime: Tokio.
- CLI parsing: clap derive in `src/cli.rs` and `src/command/*`.
- Database: SQLite through Sea-ORM.
- Serialization: serde and serde_json.
- Logging/diagnostics: tracing and user-facing `CliError`/`CliResult`.
- Frontend: Next.js 16, React 19, TypeScript, Tailwind CSS, pnpm.
- Prefer existing helpers and local patterns over new abstractions. In particular,
  use `src/utils/`, `src/internal/db.rs`, command test helpers, and AI runtime
  helper APIs where they already model the behavior being changed.

## Rust coding rules

- Run `cargo +nightly fmt --all` formatting. `rustfmt.toml` groups imports as
  standard, external, then crate imports.
- CI treats clippy warnings as failures:
  `cargo clippy --all-targets --all-features -- -D warnings`.
- Avoid wildcard imports except in tests.
- Prefer `anyhow::Result`/`anyhow::Context` for CLI flows and `thiserror` for
  domain/library errors. User-facing errors must say what failed, which resource
  was affected, and what the user can do next.
- Production code must not use `unwrap()`, `expect()`, or `panic!()` unless the
  case is obviously infallible and has a short `// INVARIANT:` comment. This
  applies to startup and initialization paths too. Tests may use them.
- When adding public enum contracts under `src/internal/ai/agent_run/`, preserve
  additive compatibility with `#[non_exhaustive]` where the existing guard expects
  it.
- Respect the repository object format. Libra supports `sha1` and `sha256` via
  `core.objectformat`; do not hard-code 20-byte object IDs.
- Keep hot paths streaming and bounded. Avoid unbounded directory walks, retries,
  buffers, allocations, or network calls in command paths unless clearly justified.
- Do not log secrets, tokens, provider keys, vault material, full authorization
  headers, or sensitive AI transcript details. Use existing redaction utilities.

## Build and test commands

Use `LIBRA_SKIP_WEB_BUILD=1` for Rust-only iteration when the embedded Code UI is
not the subject of the change.

```bash
cargo +nightly fmt --all --check
LIBRA_SKIP_WEB_BUILD=1 cargo clippy --all-targets --all-features -- -D warnings
LIBRA_SKIP_WEB_BUILD=1 cargo test --all
cargo run -- <cmd>
```

CI's `compat-offline-core` job runs the default-feature suite (step "Run tests (L1 + L2 + L3)",
with GitHub/DeepSeek/D1/R2 secrets injected; `required-features` targets excluded) as
`cargo nextest run --all --no-fail-fast --retries 2` (pinned `cargo-nextest 0.9.143`;
test groups come from the generated `.config/nextest.toml` — regenerate with
`sh tests/NEXTEST_GROUPS.sh` after touching `tests/SERIAL_REGISTRY.tsv`).
`cargo test --all` remains the acceptance gate.

Feature-gated Rust tests:

```bash
cargo test --features test-network --test network_remotes_test -- --test-threads=1
cargo test --features test-live-ai --test ai_agent_test --test ai_chat_agent_test -- --test-threads=1
LIBRA_ENABLE_TEST_LIVE_CLOUD=1 cargo test --features test-live-cloud --test cloud_storage_backup_test --test publish_live_test --test storage_r2_test -- --test-threads=1
LIBRA_ENABLE_TEST_PROVIDER=1 cargo test --features test-provider --test code_ui_scenarios --test harness_self_test -- --test-threads=1   # local-only; CI dropped this step on 2026-08-31
cargo test --features otlp --test otlp_telemetry -- --test-threads=1
cargo test --features keyring --test auth_keyring_backend -- --test-threads=1
LIBRA_TEST=1 cargo test --features test-upgrade --test upgrade_auto_test --test upgrade_publish_contract_test -- --test-threads=1
```

Web UI checks:

```bash
pnpm --dir web install --frozen-lockfile
pnpm --dir web lint
pnpm --dir web build
pnpm --dir web test
```

Worker checks:

```bash
pnpm --dir worker lint
pnpm --dir worker test
pnpm --dir worker test:miniflare
pnpm --dir worker build
```

## Tests and docs synchronization

- Pair new or changed command behavior with focused tests in `tests/command/` and,
  when behavior crosses command boundaries, a compat guard in `tests/compat/`.
- When adding or renaming a top-level integration test target, update
  `tests/INDEX.md` in the same change.
- When adding a file under `tests/compat/`, add the corresponding `[[test]]`
  entry in `Cargo.toml` and update `tests/compat/README.md`.
- CLI help and docs are part of the public contract. New visible commands/flags
  need examples, useful flag descriptions, and matching docs under
  `docs/commands/` when user-facing. Every visible command ships a
  `pub const <CMD>_EXAMPLES` wired via `#[command(after_help = …)]`; the
  `compat_help_examples_banner`, `cli::tests::root_after_help_lists_every_visible_command`,
  and `compat_command_docs_examples_section` guards enforce this.
- New stable error codes such as `LBR-*-NNN` must be documented in
  `docs/error-codes.md`.
- Schema changes need SQL migrations under `sql/migrations/` or `sql/publish/`
  as appropriate, plus migration/bootstrap tests.
- Externally visible behavior changes should update README, command docs,
  compatibility docs, or migration notes as needed.

## AI runtime and security-sensitive code

- Treat `src/internal/ai/agent/`, `src/internal/ai/tools/`,
  `src/internal/ai/permission/`, `src/internal/ai/sandbox/`,
  `src/internal/ai/session/`, `src/internal/ai/providers/`, and
  `src/command/code*` as security- and reliability-sensitive.
- Preserve approval, sandbox, ACL, origin/token, redaction, cancellation, session
  durability, and file-history authority checks unless a test explicitly proves
  the new contract.
- Live provider tests must remain feature- and secret-gated. Missing credentials
  should skip with an explicit message, not fail or silently switch to a costlier
  provider/model.
- For provider changes, keep request/response transforms, retry taxonomy,
  streaming behavior, and context-overflow compaction covered by the relevant
  `ai_provider_*` tests.

## Frontend and Worker guidance

- `build.rs` generates `web/out/`, which `src/command/web_assets.rs` embeds with `rust-embed`; setting
  `LIBRA_SKIP_WEB_BUILD=1` writes a stub output for Rust-only builds. When
  changing `web/`, run the real `pnpm --dir web build`; `web/out/` is generated
  and ignored, so never add its static export to a commit.
- The `web/` UI is an operational Code UI, not a marketing landing page. Favor
  dense, predictable controls and existing components in `web/src/components/`.
- The `worker/` app serves publish snapshots from D1/R2. Validate request input,
  preserve redaction and access checks, and keep wire types synchronized with the
  Rust publish pipeline.

## Git and PR workflow

- This working tree is a Libra checkout (`.libra/`, no `.git/`), so version-control
  operations go through Libra commands (`git` does not work here):
  `libra status`, `libra add`, `libra commit -a -s -m "<scope>: ..."`,
  `libra push origin <branch>`.
- Commit messages should use short conventional summaries such as
  `feat(status): support porcelain v2` or `fix(push): record tracking reflog`.
- Commits are expected to be DCO-signed and PGP-signed:
  `libra commit -s -m "scope: message"` (`-s` adds the DCO trailer; Git's `-S` is
  not exposed — GPG signing comes from the repository vault key via
  `vault.signing` / `commit.gpgSign`, see `--no-gpg-sign` in `libra commit --help`).
- PR descriptions should list intent, linked issues, user-visible behavior,
  tests run, and compatibility or migration impact.

## How Copilot should assist

- Ground suggestions in the actual files in this repository. Do not invent
  modules or crates that are not present.
- For CLI work, include clap wiring, handler logic, JSON/machine output behavior
  where relevant, docs updates, and integration tests.
- For AI/runtime work, include failure modes, cancellation, persistence,
  redaction, permissions, and deterministic tests before suggesting live tests.
- For storage, cloud, and publish work, consider local fallback behavior, D1/R2
  schema compatibility, retries, object integrity, and resource cleanup.
- For reviews, use a high-recall production-risk mindset: flag plausible
  security, correctness, data-loss, compatibility, performance, missing-test, and
  missing-doc issues. Flag any production `unwrap()`/`expect()` without an
  invariant comment.

## Non-goals

- Do not recommend moving core VCS, AI runtime, or storage logic out of Rust.
- Do not bypass Git compatibility unless the user explicitly requests a
  Libra-only behavior and tests/docs mark the boundary.
- Do not add live network, live AI, or live cloud dependencies to default tests.
- Do not weaken approval, sandbox, redaction, or secret-handling guarantees for
  convenience.
