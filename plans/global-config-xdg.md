# Plan: move the global config store from `~/.libra/config.db` to `.config/libra/`

> **SUPERSEDED（2026-09-19）：** 本文件是规划过程草稿；正式交付已按仓库模板 v2.6 写成
> [`docs/development/plan/plan-20260919.md`](../docs/development/plan/plan-20260919.md)（含 4 张卡、
> ADR-GCX-01..07、依赖登记与发布窗口），并已在 `plan-long.md` 登记。用户裁决：目标目录
> `<XDG_CONFIG_HOME|~/.config>/libra`（macOS 同）、旧库首次使用自动迁移（策略 A）、全域
> vault unseal key 随迁、`~/.libra` 仍为 `LIBRA_HOME`、`patch` 发布。

Status: **superseded by docs/development/plan/plan-20260919.md**
Date: 2026-09-18
Scope: Libra global configuration storage location (user scope only)

## Context

Today every per-user configuration lives inside the Libra install/home directory:

- `ConfigScope::Global` resolves to `~/.libra/config.db` (or `LIBRA_CONFIG_GLOBAL_DB`).
- `~/.libra/` also holds non-config state: `bin/` (installed binary), `env` / `env.fish`
  (installer shell hooks), `seccomp.bpf` (sandbox asset), `upgrade/` (self-upgrade state),
  `vault-keys/<repo-id>` (per-repo unseal keys), `tmp/` (vault SSH temp files).

Problems with the current layout:

1. Configuration is mixed with install and upgrade state, so users, backup/sync tools and
   dotfile managers cannot separate "my settings" from "the app's binaries and state".
2. The XDG convention (`$XDG_CONFIG_HOME/libra`, default `~/.config/libra`) is already used
   for one user-global file: `dirs::config_dir()/libra/hooks.json`
   (`src/internal/ai/hooks/config.rs:118-127`). The config DB does not follow it.
3. `libra init` has to guard against a repository being created inside `~/.libra`
   (`src/utils/util.rs:160-175`, `src/command/init.rs:551-565`) precisely because config and
   home state share one directory.

Intended outcome: global configuration is read/written at
`<XDG config dir>/libra/config.db` (on Linux `~/.config/libra/config.db`), with a defined,
tested compatibility path for existing `~/.libra/config.db` users, while `~/.libra` remains
the Libra home for binaries, upgrade state and installer files.

## Decisions still open (see Questions)

- Target path resolution: `dirs::config_dir()` vs literal `$HOME/.config`.
- Legacy DB policy: auto-migrate / explicit migrate command / read-only fallback / hard cut.
- Whether `vault-keys/` and `tmp/` move too or stay under `~/.libra`.
- `LIBRA_HOME` vs `LIBRA_CONFIG_GLOBAL_DB` decoupling.
- Release level (`patch` compat window vs `minor`).

## Current state (verified anchors)

| Fact | Evidence |
|---|---|
| Global scope path + env override | `src/command/config.rs:78-120` (`ConfigScope::get_config_path`) |
| Second global path resolver (config cascade) | `src/internal/config.rs:1261-1272` (`global_config_path`) |
| Third global path resolver (storage/cloud) | `src/utils/client_storage.rs:2556-2565` (`storage_global_config_path`) |
| Libra home resolution, `LIBRA_HOME`, and the `LIBRA_CONFIG_GLOBAL_DB`-parent rule | `src/internal/upgrade/home.rs:1-90` |
| Per-repo unseal keys under `~/.libra/vault-keys/<repo-id>` (0600/0700) | `src/internal/vault.rs:868-925` |
| Vault SSH temp files under `~/.libra/tmp` | `src/command/fetch.rs:355-390`, `:461-465` |
| "Reserved per-user state" guard + refusal to init at home | `src/utils/util.rs:160-175`; `src/command/init.rs:551-565` |
| Existing XDG precedent for the user-global hooks file | `src/internal/ai/hooks/config.rs:118-127` |
| Doctor reports `path_source` = `LIBRA_CONFIG_GLOBAL_DB` \| `home` | `src/command/config/doctor.rs:60-95`, `:338` |
| Confirmed global repair resolves through `ConfigScope::Global` | `src/command/config/repair.rs:165-180` |
| Test isolation fixture sets `LIBRA_CONFIG_GLOBAL_DB`, `LIBRA_CONFIG_SYSTEM_DB`, `XDG_CONFIG_HOME`, `HOME` | `src/utils/test.rs:200-250` |
| `dirs = "6.0.0"` already used across the tree | `Cargo.toml:101` |
| Installer keeps `LIBRA_HOME="${LIBRA_HOME:-$HOME/.libra}"` | `install.sh:14-16`, `:197-244` |

## Approach (recommended, pending answers)

1. **Single source of truth for the path.** Add one resolver (e.g.
   `internal::config::global_config_dir()` / `global_config_path()`) that:
   - honors `LIBRA_CONFIG_GLOBAL_DB` first (unchanged, tests depend on it),
   - otherwise uses `<config_dir>/libra/config.db`,
   - otherwise falls back to the legacy `~/.libra/config.db` per the chosen policy.
   Replace the three duplicate resolvers with calls into it (`GC-02` single owner).
2. **Legacy compatibility.** Per the chosen policy, keep existing `~/.libra/config.db` data
   reachable and never silently discard it. Any migration must be:
   file-level, verified (row counts / SQLite integrity + schema receipt), atomic
   (temp copy + rename), with the legacy file preserved as a backup, and must respect
   GC-13 (configuration role only — no repository tables, no global version drift).
3. **Decouple `LIBRA_HOME` from the config path.** `LIBRA_HOME` (default `~/.libra`)
   continues to own `bin/`, `env`, `seccomp.bpf`, `upgrade/`. `LIBRA_CONFIG_GLOBAL_DB`
   remains a pure DB-path override. The "parent of `LIBRA_CONFIG_GLOBAL_DB` is the Libra
   home" rule in `upgrade::home::resolve_libra_home()` must be revisited so an XDG default
   cannot silently relocate upgrade state.
4. **Surface the change.** `libra config path` reports the active path (and the legacy path
   when one exists); `libra config doctor --global-schema` reports
   `path_source = env | xdg | legacy | home`; a one-time deprecation warning points users at
   the migration command when the legacy path is still in use.
5. **Docs + tests.** Update every doc that documents `~/.libra/config.db`, and add tests for
   path resolution, migration idempotency, fallback precedence, and role isolation.

## Files to modify (draft; to be finalized)

Implementation:
- `src/internal/config.rs` — canonical resolver (global + system paths), cascade call sites.
- `src/command/config.rs` — `ConfigScope::get_config_path`, `config path` output, warnings.
- `src/utils/client_storage.rs` — drop the duplicate resolver, delegate.
- `src/internal/upgrade/home.rs` — decouple `LIBRA_HOME` from the config DB parent.
- `src/internal/db.rs` — doc/comment + cache-key behavior if needed.
- `src/command/config/doctor.rs` — `path_source`/legacy reporting (additive JSON).
- `src/command/config/repair.rs` — if repair must target legacy/new explicitly.
- `src/command/init.rs`, `src/utils/util.rs` — reserved-home guard against the new config dir.
- `src/internal/vault.rs`, `src/command/fetch.rs` — only if the secret/tmp scope is included.
- New migration helper (module + command, e.g. `src/command/config/migrate.rs`) per policy.

Tests:
- `src/utils/test.rs` — fixture defaults for the new layout.
- `tests/command/config_test.rs` (`:1561` currently asserts `home/.libra/config.db`),
  `tests/command/init_test.rs` comments, `tests/command/commit_editor_test.rs`,
  `tests/compat/libra_hooks_lifecycle_test.rs`, `rev_parse_peel_selectors_test.rs`,
  `diff_review_options_test.rs`, `config_defaults_commit_status_failures.rs`,
  `src/utils/util.rs` adoption-guard tests.
- New: path-resolution unit tests, legacy fallback/migration tests, doctor `path_source`
  tests, isolation test asserting repo operations never touch `~/.config/libra`.

Docs (42 occurrences of `~/.libra/config.db` today):
- `docs/commands/config.md` + `docs/commands/zh-CN/config.md`
  (scope table `:33`, path notes `:59`, `:74`, doctor section `:84-89`, repair `:130`).
- `docs/development/commands/config.md`, `docs/development/account.md`,
  `docs/development/commands/init.md`, `docs/commands/init.md` + zh-CN,
  `docs/commands/{pull,cloud,push,fetch,clone}.md` zh-CN where referenced,
  `COMPATIBILITY.md` if the `config` row mentions the path, website mirror pages.
- `.env.test.example` / README if they document the global DB override.

## Reuse (existing utilities)

- `dirs::config_dir()` precedent: `src/internal/ai/hooks/config.rs:122`.
- `ScopedConfig` / `ConfigScope` connection caching and schema receipts:
  `src/command/config.rs:120-200`, `src/internal/db.rs`.
- Test isolation: `ConfigDbFixture` (`src/utils/test.rs:200-250`) and `ScopedEnvVar`.
- Schema/role machinery from GC-13 and `src/internal/db/migration*` (configuration ledger).
- Repair/doctor patterns: `src/command/config/doctor.rs`, `src/command/config/repair.rs`.
- Installer home contract to keep in lockstep: `install.sh:14-16`.

## Steps (implementation checklist, draft)

- [ ] Resolve open questions (target dir, legacy policy, secret/tmp scope, LIBRA_HOME, release level).
- [ ] Add one canonical global-config path resolver + unit tests (env > xdg > legacy policy).
- [ ] Replace the three duplicate resolvers and keep behavior for `LIBRA_CONFIG_GLOBAL_DB`.
- [ ] Implement the chosen legacy policy (migration command and/or fallback + warning) with atomic, verified copy; register it in docs and `libra config doctor`.
- [ ] Decouple `resolve_libra_home()` from the config DB parent; update its tests and `install.sh` docs if needed.
- [ ] Extend `libra config path` / `doctor --global-schema` output (`path_source`, legacy path); keep JSON additive.
- [ ] Update reserved-home guards (`init`, `is_global_libra_home`) to cover the new config dir.
- [ ] Update docs (EN + zh-CN + website) and `COMPATIBILITY.md` if applicable.
- [ ] Tests: path resolution, migration idempotency + failure injection, fallback precedence, role isolation, existing tests updated.
- [ ] Verify with `cargo +nightly fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`, focused `command_test config_test`, then the plan's full gate.

## Verification (draft)

1. Fresh HOME: `HOME=$(mktemp -d) libra config set --global user.email a@b.c` creates
   `$HOME/.config/libra/config.db` and never creates `$HOME/.libra/config.db`.
2. Legacy user: with only `~/.libra/config.db` present, `libra config get --global user.email`
   still returns the value (fallback or migration per chosen policy), and after migration the
   new file contains the same rows and the legacy file is preserved.
3. `libra config path --global` prints the active path; `--json` adds the legacy path field.
4. `libra config doctor --global-schema` reports `path_source = env|xdg|legacy` and the
   legacy ledger classification without touching `~/.libra/upgrade`.
5. `XDG_CONFIG_HOME=/tmp/xdg libra config path --global` → `/tmp/xdg/libra/config.db`.
6. Isolation: repository commands (`libra status`, `libra add`, `libra commit`) with a
   sandbox HOME never create or read `$HOME/.config/libra/config.db` unless global config is
   actually consulted.
7. Full gate: `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`.

## Open questions

1. Target path on non-Linux: `dirs::config_dir()` (macOS `~/Library/Application Support/libra`,
   Windows `%APPDATA%\libra`) or literal `$HOME/.config/libra` everywhere?
2. Legacy `~/.libra/config.db`: auto-migrate on first use, explicit `libra config migrate-global`
   (+ read-only fallback warning), read-only fallback only, or hard cut?
3. Scope: only the config DB, or also `vault-keys/` and `tmp/`?
4. Keep `~/.libra` as the Libra home for binaries/upgrade and decouple
   `LIBRA_CONFIG_GLOBAL_DB` from it (the current parent-directory rule)?
5. Release level: `patch` with a compatibility window, or `minor` as a breaking change?
