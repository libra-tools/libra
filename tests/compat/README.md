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
| `subface_labels.rs` | CG-01 (plan-20260708) | `COMPATIBILITY.md` 的「Sub-face compatibility grading」矩阵机器校验：子面标签限于固定五枚举、被分级命令集钉死在 P0/P1 触达面且不脱离 `src/cli.rs::Commands`、同一命令不得把一个子面分进两档、每个 `unsupported` 子面带治理编号并与 `_compatibility.md` 登记表双向一致 |
| `conflict_status_diff_test.rs` | P0-01 (plan-20260708) | merge / rebase / cherry-pick 内容冲突后，`status --porcelain` 输出 `UU`、porcelain v2 输出 `u UU ...`、`ls-files -u/-t` 暴露 stage 1/2/3，`diff` 使用 `diff --cc` 而不是把冲突文件误报为 `/dev/null` 新增 |
| `diff_check_safety_test.rs` | P0-02 (plan-20260708) | `diff --check` 覆盖 Git 的三类安全检查：尾随空白、leftover conflict marker、new blank line at EOF，且任一命中退出码为 2 |
| `diff_review_options_test.rs` | P1-08a (plan-20260708) | `diff --raw` A/D/M/R/T 与 NUL rename 字段、工作树侧 zero OID/纯 mode 方向 ID、`--summary` mode change、external-driver mode/hash、content+mode/executable-content unified/full-index header、textconv/空白 body suppression 后保留 mode、rename 不跨 regular/symlink file type 配对、`--compact-summary`、`--diff-filter` include/exclude/真正 all-or-none 及 sparse-view 投影后重判、`--full-index`、CLI src/dst prefix 与坏默认短路 |
| `clone_shallow_integrity_test.rs` | P0-03 (plan-20260708) | 本地 Libra 源的 `clone --depth` / `fetch --depth` 必须 fail-closed（`LBR-REPO-002`）且不留下 broken target / shallow metadata；`rev-parse --is-shallow-repository` 正确报告 shallow 布尔 |
| `checkout_branch_startpoint_test.rs` | P0-04 (plan-20260708) | `checkout -b/-B <branch> <start-point>` 与 `switch -C <branch> <start-point>` 必须把 `HEAD` 保持为目标分支的 symbolic ref；无效 start-point 必须 fail-closed 且不移动 `HEAD` / 既有分支引用 |
| `switch_orphan_root_test.rs` | P0-05 (plan-20260708) | `switch --orphan` / `checkout --orphan` 必须把 `HEAD` 指向 unborn 分支、保留 index/worktree、JSON 标记 `unborn=true`，并让首个用户提交成为无 parent 的 root commit；已有分支和不支持的 start-point 必须 fail-closed |
| `broken_pipe_output_test.rs` | P0-06 (plan-20260708) | `log`、`diff`、`grep`、`ls-files`、`show`、`for-each-ref` 等 stdout 命令在下游提前关闭管道时必须静默正常结束，不打印 panic/backtrace/BrokenPipe 噪声 |
| `commit_amend_no_edit_test.rs` | P0-07 (plan-20260708) | clean `commit --amend --no-edit` 必须真正重写 HEAD，保留 tree/parents/message，并刷新 committer date；不得打印成功但保持 HEAD 不变 |
| `commit_identity_date_test.rs` | P0-08 (plan-20260708) | `commit` 必须支持 Git author/committer 身份与日期环境变量、`--date`、`--reset-author`，并让 `-C/-c` 复用来源提交的 message 与 author metadata |
| `sequencer_message_author_test.rs` | P0-08 (plan-20260708) | `cherry-pick` 必须保留原提交 author metadata，`revert` 必须使用当前身份创建提交，且二者生成消息不得从签名块派生错误 subject |
| `write_tree_missing_object_test.rs` | P0-09 (plan-20260708) | `write-tree` / `commit` 在写 tree 或 commit 前必须拒绝 index 中缺失或类型不匹配的对象（`LBR-REPO-002`），且失败不得移动 `HEAD` |
| `init_shared_mode_test.rs` | P0-10 (plan-20260708) | `init --shared=<numeric>` 必须预拒绝不可遍历目录权限且不留下半仓库；`group`/`all`/可用 numeric 模式必须持久化 `core.sharedRepository`，reinit 同步更新该配置 |
| `symlink_basic_test.rs` | P0-11 (plan-20260708) | symlink 必须以 index mode `120000` 和 link target blob 入库；pathspec reset 必须保留 symlink index mode；checkout/restore/reset 必须恢复真实 symlink；status/diff/ls-files 必须识别 symlink target 变更且 dangling symlink 不误报删除；非 Unix 平台必须显式诊断而非写普通文件 |
| `global_config_schema_future_test.rs` | P0-12 (plan-20260708) | 全局 config DB schema 比当前二进制新时，`pull` 等远端/云命令默认 fail-closed 并输出 `LBR-CONFIG-001`；`--offline` / `LIBRA_READ_POLICY=offline|local` 明确降级；完整进程环境或 repo-local `LIBRA_STORAGE_*` 配置不误报；本地命令只 warning；JSON/人类诊断包含升级命令且不泄露 vault secret |
| `pathspec_magic_test.rs` | P1-01 (plan-20260708) | 共享 pathspec parser/matcher 必须支持 `top` / `exclude` / `icase` / `literal` / `glob` magic、子目录相对解析，并被 `ls-files` / `grep` / `diff` / `status` 只读消费者复用 |
| `ignore_attributes_sources_test.rs` | P1-02 (plan-20260708) | Git 标准 ignore/attributes 来源（`.gitignore`、`.git/info/*`、`core.*File`）与 Libra 扩展来源并存；覆盖 `status` / `add` / `clean` / `check-ignore` / `check-attr` / `lfs` / `diff --textconv` / `archive export-ignore` |
| `machine_porcelain_contract_test.rs` | P1-03 (plan-20260708) | 机器可读 porcelain 契约：`status --porcelain=v1/v2 -z` 使用 NUL 记录、默认 `diff` 不含 untracked 且 `--quiet`/`--exit-code` 退出码正确、`ls-files --error-unmatch` 退出 1、`grep` 命中/无命中/错误分别退出 0/1/2 |
| `pretty_format_placeholders_test.rs` | P1-04 (plan-20260708) | `log` / `show` / `shortlog` 共享 Git-like pretty-format placeholders（含 ASCII/control `%xNN`、`%%` 与 forced color）；`log --name-only --format` 分隔与 `log -z --name-status` NUL 记录对齐 Git |
| `config_defaults_semantics_test.rs` | P1-05a/P1-05c/P1-05d/P1-05e (plan-20260708) | 高影响 Git config 默认值：`init.defaultBranch`、`pull.rebase` / `branch.<name>.rebase`、`pull.ff=true|false|only` 与 `fetch.prune` / `remote.<name>.prune`（远程键跨 scope 优先、Git 数值布尔、CLI 覆盖、`--all` 联网前整体校验）覆盖 local/global/system、变量名大小写、空/无效值、真实 rebase/prune 行为及 fetch 前 fail-closed；`status.*` 展示默认（untracked 三态、short/branch 仅塑形人类 short、showStash、relativePaths，porcelain 免疫 + 输出前 fail-closed）；`branch.sort`/`tag.sort`（`--sort` 恒胜、branch 配置不隐含 list/不抑制 unborn-HEAD、tag 配置不翻转创建、未设时 tag 按 refname 升序、多值取胜出 scope 最后一个、不可读配置库 LBR-IO-001）；`diff.context`/`diff.renames`（Git `int` 范围与 k/m/g 后缀、默认开启重命名、严格三级级联、`-U`/`-M`/`--no-renames` 恒胜、`copies`/`copy` 真实退化分支、稳定错误码先于进度/内容输出）；`diff.noPrefix`/`diff.mnemonicPrefix`/`diff.srcPrefix`/`diff.dstPrefix`（严格级联与布尔校验、Git 优先级、全部 mnemonic 组合、反转/暂存区/相对路径/重命名/plumbing、binary `/dev/null`、CRLF/word-diff 内容隔离、local/global 读取失败先于输出且 system scope 失败跳过）；`format.pretty`/`log.date`/`log.follow`（log/show CLI 优先级、严格错误、单路径 human+JSON follow、子目录规范化与 exact-blob 重命名遍历）；`commit.status`（默认 true、严格三级级联、CLI/无编辑器/非剥离 cleanup 绕过，invalid 与不可读 store 均覆盖 `-m`/dry-run/porcelain/JSON/non-strip/显式关闭，模板错误早于 hook/editor/history，dry-run task-local 隔离 index + 非 verbose 流式哈希 + verbose 对变化的 HEAD/已暂存/auto-stage 的读前字节与对象数预算（含真实 CLI 聚合/4096 唯一对象拒绝）+ linked worktree 共用仓库 scratch 配额/清理 + non-verbose 无 scratch 写依赖 + loose 严格流式边界校验 + 完整 delta 链计费/非法指令拒绝 + pack 单次枚举/每 index 单次打开且不重建缺失 index，零对象写入且跳过 hook/editor/rerere/post_commit automation，真实 auto-stage status 失败保留 object-valid regular/LFS index，真实 LFS persist 失败返回 LBR-IO-002 且 index 不变）|
| `config_defaults_edge_cases_test.rs` | P1-05a (plan-20260708) | 加密 local/global 默认值先解密、不可读/不支持 system scope 跳过、Git 转换报告源 `HEAD` 分支，以及配置默认值边界回归 |
| `config_history_defaults_test.rs` | P1-05b (plan-20260708) | 历史相关默认值：`merge.ff`、`merge.log`、`merge.verifySignatures`、`commit.gpgSign` 与 CLI override 优先级 |
| `fetch_remote_refspec_test.rs` | P1-06 (plan-20260708) | 显式/配置 fetch refspec 精确映射、FETCH_HEAD/remote HEAD 元数据、`remotes.default`、多 ref 事务回滚、remote rename tracking namespace 迁移与 `ls-remote --symref` |
| `noninteractive_history_controls_test.rs` | P1-07a/P1-07b/P1-07c (plan-20260708) | rebase 非交互 controls；merge `-s ours`、hunk-level `-X`、无关历史与 `--log`；cherry-pick/revert last-wins hunk-level `-X`、revert cleanup conflict→continue 与 corrupt-index fail-closed，以及 reset merge/keep staged/unstaged 保留或拒绝、untracked collision、file/directory 转换、symlink ancestor no-follow 和原子回滚（Unix 33 E2E） |

## Authoring guidelines

- **Do** assert outward contracts: `--help` strings, JSON schema keys, exit
  codes that other tools (CI scripts, wrappers) depend on.
- **Don't** duplicate per-command happy/error paths — those belong in
  `tests/command/<name>_test.rs`.
- Use the same test helpers as `tests/command/*` (see
  [`tests/command/mod.rs`](../command/mod.rs)).
- Cross-platform tests (worktree dir deletion, etc.) should annotate
  platform-specific differences with `cfg(unix)` / `cfg(windows)`.
