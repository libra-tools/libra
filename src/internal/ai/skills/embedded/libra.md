---
name = "libra"
description = "Core knowledge and safe workflows for working inside any libra-format repository (the AI-agent-native VCS). Use /skill libra early when the tree contains .libra/ instead of .git/."
version = "1.0.0"
allowed-tools = [
  "read_file",
  "list_dir",
  "grep_files",
  "search_files",
  "web_search",
  "shell",
  "apply_patch",
  "run_libra_vcs",
  "update_plan",
  "submit_task_complete"
]
---

# Working with libra-format repositories

Libra is a **single-crate** (Rust 2024) Git-compatible VCS that is purpose-built for AI agents. It maintains on-disk object/pack/index compatibility with Git while using SQLite for all transactional metadata and AI runtime state.

## Layout — never assume a .git/ tree

- **Metadata**: `.libra/libra.db` (SQLite via SeaORM) holds:
  - Git core: `config`, `config_kv`, `reference`, `reflog`, `rebase_state`, `object_index`, `schema_version`
  - AI runtime: `ai_thread*`, scheduler tables, `ai_index_*`, `ai_decision_*`, live context windows, etc.
  - `.libra/vault.db` stores secrets (libvault).
- **Objects & compatibility layer**: standard Git layout (`objects/`, `index`, `pack/`, `info/`, `logs/HEAD`, etc.) still exists for `git` interop and `libra` commands that read loose/pack data directly.
- **Worktrees / submodules**: normal Git rules apply on top of the above.

When you see `.libra/libra.db`, you are inside a libra repo. Do **not** run raw `git` commands against it unless you explicitly want to exercise the compatibility surface.

## Process & dispatch (the real entry points)

- Binary: `src/main.rs` (tracing init + 32 MiB CLI worker thread + tokio) → `src/cli.rs::{parse,parse_async}` → `src/command/*::execute_safe`.
- Library embedding: `src/lib.rs::{exec,exec_async}`.
- `src/cli.rs` is the single source of truth for:
  - clap grammar and all public flags
  - schema preflight / migration
  - pinning `core.objectformat` (hash kind) globally for the process
  - `--json` / `--machine` output mode resolution
- All subcommands live under `src/command/`. The `execute` (or `execute_safe`) async fn is the public contract.
- AI/agent surface lives almost entirely under `src/internal/ai/` (agent, runtime, orchestrator, tools, MCP, skills, commands, prompt, providers, …).

Major boundaries you will cross often:
- `src/command/` — user-facing subcommands
- `src/internal/ai/` — the agent loop, tools, goal/supervisor, permission, sandbox, session store, etc.
- `src/internal/protocol/` — pure Git/HTTP/SSH/LFS wire clients
- `src/utils/` — storage (tiered + publish), error codes, pager, LFS, test helpers
- `web/` — Next.js frontend (static export) embedded into the binary via `rust-embed`
- `worker/` — Cloudflare Worker (OpenNext) that serves read-only `libra publish` sites

## Build, format, lint, test — the commands agents most often guess wrong

Use these **exact** invocations (they are enforced by CI and the project’s AGENTS.md):

- Format (unstable features + Std/External/Crate grouping):
  `cargo +nightly fmt --all`
- Format check:
  `cargo +nightly fmt --all --check`
- Lint gate (all targets, all features, zero warnings):
  `LIBRA_SKIP_WEB_BUILD=1 cargo clippy --all-targets --all-features -- -D warnings`
- Fast compile / check (skip the Next.js web build):
  `LIBRA_SKIP_WEB_BUILD=1 cargo check`
  `LIBRA_SKIP_WEB_BUILD=1 cargo build`
- Default test run (L1 deterministic layer only):
  `cargo test --all`
- Single integration test target (serialised):
  `cargo test --test <target> -- --test-threads=1`
- Preferred test naming in issues/PRs: `target::test_fn`
- CLI smoke: `cargo run -- <subcommand>`
- Code UI/automation tests (require the hidden test provider):
  `--features test-provider` + `LIBRA_ENABLE_TEST_PROVIDER=1` + `--test-threads=1`
- Web embed verification (the one CI job that may **not** skip the web build):
  `pnpm --dir web install --frozen-lockfile && pnpm --dir web lint && pnpm --dir web test && pnpm --dir web build`, then `cargo check --lib` against the generated `WebAssets` (compat-web-check also asserts `web/out/` is untracked and ignored — never commit the static export)
- Worker checks (inside `worker/`):
  `pnpm lint && pnpm test && pnpm test:miniflare && pnpm build`
- Required consistency check before PRs that touch surfaces (de-scripted — there is no helper script directory):
  `cargo test --test compat_matrix_alignment` (covers `COMPATIBILITY.md` ↔ `src/cli.rs::Commands` drift and `docs/commands/code.md` ↔ Code UI router coverage; also runs inside `cargo test --all`)

## Language, style & hard rules

- Edition 2024, 4-space indentation.
- `snake_case` for items, `PascalCase` for types, `SCREAMING_SNAKE_CASE` for consts.
- Imports: **Std → External → Crate** (enforced; see `rustfmt.toml`).
- **Zero `unwrap()` / `expect()` in any `src/**` production path.**
  - Tests may use them.
  - Only obviously-infallible cases (compile-time constants, etc.) are allowed and must carry a short `// INVARIANT:` comment.
- User-facing errors must be **actionable**: state what failed, the affected path/ref/object, and a concrete fix hint when known.
  - CLI flows → `anyhow::Result` + `.context(...)`
  - Domain/library errors → `thiserror` enums
- Database helpers that accept an existing connection are suffixed `_with_conn` to keep transaction safety obvious.
- Provider-specific code lives under `src/internal/ai/providers/<name>/`.

## Public surface change checklist (do all of these)

When you add or change a visible command:
1. Update `src/cli.rs` (clap definition).
2. Implement / update the handler in `src/command/<name>.rs`.
3. Add or update the row in `COMPATIBILITY.md`.
4. Write / update `docs/commands/<name>.md` (must contain an `## Examples` or `## Common Commands` section).
5. Add integration coverage under `tests/command/`.
6. Add a row to `tests/INDEX.md`.
7. Every visible command must ship a `pub const <CMD>_EXAMPLES` constant wired through clap `after_help` (or the parent subcommand binding).
8. New stable `StableErrorCode` variants must be documented in `docs/error-codes.md` (the compat guard will fail otherwise).

## Test layering & gating (L1/L2/L3)

- **L1 (default)**: pure deterministic tests — `cargo test --all`.
- **L2 (network)**: requires `LIBRA_TEST_GITHUB_TOKEN` + namespace; gate with the `env_is_present` helper + early `eprintln!("skipped (...)")`.
- **L3 (live AI / cloud)**: real API keys or D1/R2/S3 creds; same gating pattern. Never let missing secrets cause test failures.

Mark tests with a keyed `serial_test` lane — `#[serial(cwd)]`, `#[serial(env)]`, `#[serial(hash_kind)]`, or a named external key — for the process-wide resource they touch (cwd, env, ports, global config DB, …); every `global`/`lane:*` annotation needs a row in `tests/SERIAL_REGISTRY.tsv` (`compat_serial_registry` fails on drift) and `.config/nextest.toml` is regenerated with `sh tests/NEXTEST_GROUPS.sh`.

Use `tempfile::tempdir()` + `utils::test::ChangeDirGuard` (or the helpers in `tests/command/mod.rs`) so that `HOME`, `XDG_CONFIG_HOME`, `LIBRA_CONFIG_GLOBAL_DB`, `LANG` etc. are isolated.

## When you are lost

- Read the root files first: `AGENTS.md` (authoritative for agents), `CLAUDE.md`, `COMPATIBILITY.md`.
- The single best “how do I even run this” file for contributors is `CLAUDE.md` (build commands, test layers, Cargo features, environment variables).
- For the AI runtime contract and future phases, see `docs/development/tracing/agent.md`.

Activate this skill (`/skill libra`) at the start of any session that will read or modify a libra repository or the libra source tree itself. It gives you the correct mental model and the exact incantations the project expects.

Remember: the goal is not Git parity for its own sake — it is reliable, auditable, agent-first version control that still happens to be usable by humans and existing Git tooling.
