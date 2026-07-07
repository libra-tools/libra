# `tests/compat/` — cross-command compatibility regressions

This directory is the集结点 (collection point) for **cross-command** Git
compatibility regressions defined in
[`docs/development/commands/_compatibility.md`](../../docs/development/commands/_compatibility.md).

The tests in `tests/command/*_test.rs` cover each command's happy path and
error path in isolation. The tests here cover the **outward contract** stated
in [`COMPATIBILITY.md`](../../COMPATIBILITY.md): which subcommands appear in
`--help`, which Git surface flags are exposed, and which Git surface flags are
intentionally absent.

## How these tests run

`tests/compat/*` is selected by the `compat-offline-core` job in
`.github/workflows/base.yml`. The Cargo `[[test]]` integration model means each
top-level file under `tests/` becomes its own test binary. Files placed
directly under `tests/compat/` are reachable only when added as `[[test]]`
entries in `Cargo.toml` (`path = "tests/compat/<name>.rs"`); see Cargo
docs.

`tests/compat/` is now an active集结点: C2 / C4 / C5 have populated the first
surface-contract tests, and future compatibility batches should add their own
top-level `[[test]]` entries in `Cargo.toml`.

## Files

| File | Owning batch | Coverage |
|------|--------------|----------|
| `stash_subcommand_surface.rs` | C4 | `stash --help` lists `show` / `branch` / `clear`; cross-subcommand JSON schema agreement |
| `pull_strategy_flags_surface.rs` | pull recovery (2026-06-18) | `pull --help` exposes `--ff-only` / `--rebase` / `--ff` / `--no-ff` / `--depth`; deferred `--squash` / `--no-commit` / `--autostash` / `--unshallow` must NOT appear; `COMPATIBILITY.md` pull row stays aligned |
| `bisect_subcommand_surface.rs` | C4 | `bisect --help` lists `run` / `view`; EXAMPLES banner is wired |
| `worktree_delete_dir.rs` | C5 | `worktree remove` with and without `--delete-dir`; dirty-worktree refusal |
| `checkout_alias_help.rs` | C5 | top-level `--help` includes `checkout`; the help banner mentions `switch` / `restore` |
| `matrix_alignment.rs` | C2 / Web Phase E | `COMPATIBILITY.md` ↔ `src/cli.rs::Commands` enum drift detection; `docs/commands/code.md` docs script coverage for every `/api/code/*` router endpoint; Web CI checks `web/out` drift after static export |
| `live_compat_workflow.rs` | C2 | optional `compat-live-ai` / `compat-live-cloud` workflow stays manual/scheduled, secret-gated, and outside `base.yml` |
| `branch_lossy_wrapper_guard.rs` | branch follow-up | `src/` production code must use branch `*_result` APIs instead of lossy compatibility wrappers |
| `lfs_client_production_unwrap_guard.rs` | unwrap audit (v0.17.260) | `src/internal/protocol/lfs_client.rs` must not regress on bare `.unwrap()` |
| `config_production_unwrap_guard.rs` | unwrap audit (v0.17.261) | `src/internal/config.rs` must not regress on bare `.unwrap()` |
| `head_production_unwrap_guard.rs` | unwrap audit (v0.17.262) | `src/internal/head.rs` must not regress on bare `.unwrap()` |
| `util_production_unwrap_guard.rs` | unwrap audit (v0.17.264) | `src/utils/util.rs` must not regress on bare `.unwrap()` |
| `client_storage_production_unwrap_guard.rs` | unwrap audit (v0.17.264) | `src/utils/client_storage.rs` must not regress on bare `.unwrap()` |
| `extra_production_unwrap_guard.rs` | unwrap audit (v0.17.266) | extra audited files (`lfs.rs`, `object.rs`, `storage/local.rs`, `storage/tiered.rs`, `path_ext.rs`, `git_protocol.rs`, `lfs_structs.rs`, `command/reflog.rs`) must not regress |
| `all_production_unwrap_guard.rs` | unwrap audit (v0.17.268) | catch-all guard walking the entire `src/` tree; new modules are automatically in scope |
| `agent_run_non_exhaustive_guard.rs` | agent_run | every `pub enum` exposed under `src/internal/ai/agent_run/` must carry `#[non_exhaustive]` so additive evolution is non-breaking |
| `agent_docs_contract.rs` | agent plan docs | `docs/development/tracing/agent.md` must not claim removed provider surfaces still exist, drop public schema/retention/raw-export constraints, or link stale internal-plan files |
| `agent_capability_matrix_pin.rs` | AG-16 capability contract | E1 `DeclaredAgentCaps` serializes exactly 8 snake_case keys; first-batch roster frozen to `claude-code`/`codex`/`opencode`; unsupported/unknown agents never installable or launchable |
| `agent_architecture_guard.rs` | AG-16 architecture boundary | observed_agents must not import AgentRuntime/checkpoint layers; `agent_for` total over `AgentKind`; static roster is built-in-only; SQL CHECK constraint and doc roster stay in sync with the enum |
| `help_examples_banner.rs` | cross-cutting item B (v0.17.841) | every visible command in `src/cli.rs::Commands` renders `EXAMPLES:` / `Examples:` in `<cmd> --help` |
| `error_codes_doc_sync.rs` | cross-cutting (v0.17.842) | every `LBR-*-NNN` literal in `src/utils/error.rs` is documented in `docs/error-codes.md` |
| `command_docs_examples_section.rs` | cross-cutting item B (v0.17.851) | every `docs/commands/<name>.md` page carries an `## Examples` / `## Common Commands` heading |
| `help_flag_descriptions.rs` | cross-cutting item B (v0.17.887, extended v0.17.900 / v0.17.902 / v0.17.904) | every visible flag and positional argument under `Options:` / `Arguments:` in `libra <cmd> --help` carries a non-empty description line — scans 42 root commands + 53 sub/sub-sub commands (110 surfaces). Rejects clap auto-annotations like `[default: ...]` masquerading as descriptions |
| `help_no_impl_meta_leak.rs` | cross-cutting item B (v0.17.894, extended v0.17.901 / v0.17.911) | no `libra <cmd> --help` body contains contributor-facing rustdoc that should not have leaked into clap's `long_about`. Currently forbids 6 phrase classes: `for the same EXAMPLES rendered through clap`, `for the same examples rendered through clap`, `CLI arguments for the`, `type is wired into the top-level CLI`, `Codex pass-`, `\`\`\`text `, and `# Examples` (raw markdown heading + code fence) |
| `format_patch_flag_surface.rs` | format-patch (2026-06-20) | `format-patch --help` lists `--output-directory`, `--stdout`, `--numbered`, `--start-number`, `--subject-prefix`, `--cover-letter`, `--thread`/`--no-thread`, `--in-reply-to`, `--reroll-count`, `--signoff`, `--full-index`, `--no-stat`, `--keep-subject`, `--suffix`, `--zero-commit`, `--signature`, `--no-signature`, `--numbered-files` and `revision-range`; EXAMPLES banner is wired |
| `otlp_feature_gate_guard.rs` | `compat_otlp_feature_gate_guard` | lore.md 1.7 硬约束：`otlp` feature 不得进入 default、四个 opentelemetry 依赖保持 optional、模块声明与 main.rs 使用点保持 `#[cfg(feature = "otlp")]` 门控 |
| `keyring_feature_gate_guard.rs` | `compat_keyring_feature_gate_guard` | lore.md 2.7 门控：`keyring` feature 不入 default、依赖 optional + VENDORED libdbus（静态——终端用户无运行时 dylib 依赖）、后端模块 cfg 门控（发布构建显式 --features keyring 启用） |
| `fastcdc_feature_gate_guard.rs` | `compat_fastcdc_feature_gate_guard` | lore.md §6 硬约束：`fastcdc` FastCDC media chunking feature 不入 default、保持纯 in-tree（`fastcdc = []` 无捆绑依赖）、`utils::media`/`command::media` 模块声明与 cli.rs `Media` 变体+dispatch 保持 `#[cfg(feature = "fastcdc")]` 门控 |

## Authoring guidelines

- **Do** assert outward contracts: `--help` strings, JSON schema keys, exit
  codes that other tools (CI scripts, wrappers) depend on.
- **Don't** duplicate per-command happy/error paths — those belong in
  `tests/command/<name>_test.rs`.
- Use the same test helpers as `tests/command/*` (see
  [`tests/command/mod.rs`](../command/mod.rs)).
- Cross-platform tests (worktree dir deletion, etc.) should annotate
  platform-specific differences with `cfg(unix)` / `cfg(windows)`.
