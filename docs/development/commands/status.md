# `libra status` 开发设计

## 命令实现目标

`libra status` 的目标是展示工作区和索引状态，并支持 porcelain v1/v2、untracked/ignored 模式、结构化输出、`-z` NUL 终止输出、`--find-renames` 及 `--renames`/`--no-renames` 重命名检测开关、`--column`/`--no-column` 列对齐开关，以及 `--ahead-behind` 上游计数控制。

## 对比 Git 与兼容性

- 兼容级别：`supported`。

- 当前矩阵承诺常用 Git 行为已支持；`-z`、`--find-renames`、`--renames`/`--no-renames`、`--column`/`--no-column`、`--ahead-behind`/`--no-ahead-behind` 已补齐。新增语义必须同步矩阵、用户文档和测试。


## 设计方案

- 入口与分发：已公开接入 `src/cli.rs::Commands`；已由 `src/command/mod.rs` 导出。CLI 层在 `src/cli.rs` 把解析后的参数交给命令模块，命令模块负责把领域错误转换为 `CliError` / `CliResult`。
- 源码分层：主要实现文件为 `src/command/status.rs`。参数/子命令类型包括：`StatusArgs`、`PorcelainVersion`、`UntrackedFiles`；输出、错误或状态类型包括：`StatusData`（所有渲染器共享的中心数据结构，承载 staged/unstaged/unmerged/ignored/stash/upstream/merge_state/porcelain_v2 字段，模块内可见）、`Changes`、`StatusError`、`UpstreamInfo`、`MergeStatusInfo`；核心数据函数为 `collect_status_data`，辅助执行函数包括：`execute`、`execute_safe`、`execute_to`、`changes_to_be_committed_safe`、`changes_to_be_staged_split_safe`。
- 源码意图：源码模块注释说明该命令结合 ignore 策略计算 staged/unstaged/untracked 集合，并输出简洁摘要或结构化状态。
- 执行路径：`execute_safe`、`execute_to`、`collect_status_json_envelope_for_api` 三个入口都立即委托给共享数据核心 `collect_status_data(args) -> CliResult<StatusData>`（`src/command/status.rs:259`）；`execute_to` 是薄封装，先调用 `collect_status_data` 再调用 `render_status_to_writer`。索引路径会加载、比较并刷新 `.libra/index`（只读，不回写）；对象路径会解析 revision 并读取 blob/tree/commit 等对象；引用路径只读取 SQLite refs 与 HEAD（不更新 refs/HEAD，也不写 reflog）；本命令为只读，不通过 SeaORM/SQLite 或 D1 客户端持久化元数据。

- 流程图：以下流程图按当前源码分层展示主路径和底层对象边界，便于维护者把代码入口、执行函数和副作用范围对应起来。

```mermaid
flowchart TD
    A["入口与分发<br/>src/cli.rs::Commands"] --> B["源码分层<br/>src/command/status.rs"]
    B --> C["参数模型<br/>StatusArgs"]
    C --> D["执行路径<br/>execute_safe / execute_to → collect_status_data"]
    D --> E["底层对象<br/>Index / .libra/index / Blob / Commit"]
    D --> F["输出与状态<br/>StatusData / Changes / StatusError / UpstreamInfo / MergeStatusInfo"]
    E --> G["副作用边界<br/>写入分支需先预检"]
```

- 底层操作对象：`Index` / `.libra/index`（暂存区状态、路径条目和刷新/保存边界）；`Blob`（文件内容或 LFS pointer 写入对象库后的 blob 对象）；`Commit`（提交对象、父提交关系和提交消息载荷）；`TreeItem` / `TreeItemMode`（tree 中的路径项和 mode）；`Tree`（由索引或对象遍历生成的目录树对象）；`Branch` / branch store（SQLite refs 上的分支读写、过滤和上游关系）；`Head`（SQLite 中的 HEAD 指向、当前分支和 detached 状态）；SeaORM / `.libra/libra.db`（配置、refs、reflog、AI/发布元数据等 SQLite 表）；`ObjectHash`（SHA-1/SHA-256 对象 ID 和 revision 解析结果）；`ConfigKv`（配置键值持久化行）
- 输出与错误契约：人类输出、`--json` / `--machine` 输出和 quiet/verbose 分支必须继续走现有 `OutputConfig` / `emit_json_data` / `CliError` 路径；新增失败模式要补稳定错误码、用户提示和回归测试。
- 副作用边界：凡是写入索引、对象库、refs/HEAD、reflog、SQLite/D1、工作树或远端的路径，都必须先完成参数校验和 dry-run/预检分支，再执行持久化，避免部分写入后静默成功。

## 实现历史

- 本节依据本地 main 分支提交历史重写，筛选与该命令实现、测试或文档路径直接相关的提交；以下是归纳后的实现脉络。
- 2026-07-15（plan-20260708 P0-12 回归修复）：`status.*` 配置默认的 global scope 读取遇到 schema 比二进制新的全局配置库时，不再以 `LBR-IO-001` fail-closed，而是打印一次去重的 P0-12 诊断后跳过 global scope 继续（共享级联层修复，见 `docs/development/commands/config.md` 同日条目）；其它 local/global 读取失败仍为 `LBR-IO-001`。回归：`compat_global_config_schema_future::local_command_warns_once_and_continues`。
- 2025-11-11 `926b2c38`（`Add --ignored arg for libra status (#35)`）：基础实现节点：Add --ignored arg for libra status (#35)；当前实现的主要轮廓可追溯到该提交。
- 2026-06-06 `7d985dec`（`feat(status): add -z NUL-terminated porcelain output (implies v1)`）：当前 HEAD 已保留 `-z` / `--null` NUL-terminated 输出，`StatusArgs::null_terminated` 贯穿 short/porcelain 渲染路径；该能力不再作为缺口处理。
- 2025-12-10 `22ecce78`（`feat(status): support --porcelain=v2 and --untracked-files modes (#78) (#82)`）：功能演进：support --porcelain=v2 and --untracked-files modes (#78) (#82)；该节点扩展了当前命令可用的参数或行为。
- 2026-05-17 `f5351224`（`docs(status): correct porcelain-v2 rationale + document stash_entries opt-in`）：文档与兼容口径：correct porcelain-v2 rationale + document stash_entries opt-in；当前文档按该节点之后的实现状态校准。
- 2026-07-09（plan-20260708 P0-11）：源码核对确认多处工作树扫描使用 `exists()`/`is_file()`，会忽略 dangling symlink 或把 symlink 目标状态当作路径状态。当前 main scanner、split scanner 与 untracked walker 改用 `symlink_metadata` / `file_type.is_symlink()`，tracked symlink target change 会作为修改报告。回归守卫：`compat_symlink_basic`。
- 2026-07-09（plan-20260708 P1-01）：新增位置 `<pathspec>...` 并接入共享 `src/utils/pathspec/`；status 的 staged/unstaged/unmerged/ignored/untracked 集合和 merge conflict path 列表会按同一 matcher 限定。全局 merge-in-progress 状态不被 pathspec 清除，因此即使冲突路径被过滤隐藏，`--exit-code` 仍会把仓库视为 dirty，并且人类提示会说明冲突位于所选 pathspec 之外。回归覆盖：`compat_pathspec_magic` 与 `compat_conflict_status_diff`。
- 2026-07-09（plan-20260708 P1-03）：核对 `status --porcelain=v1/v2 -z` 的机器输出：记录以 NUL 终止且无尾随换行，rename-capable porcelain 在 `-z` 下不使用人读 `old -> new` 箭头。回归覆盖：`compat_machine_porcelain_contract`。
- 2026-07-11（plan-20260708 P1-05d，status 片）：`status.*` 展示默认接入严格 local→global→system 级联（`apply_status_config_defaults`，在 `execute_safe`/`execute_to` 两个入口、任何模式与输出之前统一校验五键——`status.showUntrackedFiles=no|normal|all`、`status.short`、`status.branch`、`status.showStash`、`status.relativePaths`——无效值 `LBR-CLI-002`、local/global 读取失败 `LBR-IO-001`；布尔经共享 `parse_git_config_bool`）。应用规则与 Git 对齐：CLI 恒胜；`status.short` 让位于显式 `--long`/`--porcelain`；`status.branch` 仅作用于 short 格式（porcelain 头仍需显式 `-b`，porcelain 对格式类 config 免疫）；`status.relativePaths=false` 在**渲染期**转换：采集管线保持 cwd 相对（pathspec 过滤与 porcelain-v2 元数据查找依赖它），`render_status_to_writer` 在人类 short/long 分支前经 `data_with_repo_root_paths`（`util::to_workdir_path`）克隆转换展示路径（`StatusData` 为此派生 Clone，porcelain-v2 元数据 Arc 包裹）；porcelain/JSON 路径形态不变；转换保留折叠目录的尾部 `/` 标记（R2 修正）。`/api/repo/status`（`collect_status_json_envelope_for_api`）同样先过解析器，与 `status --json` 字节等价并共享 fail-closed 校验（R2 修正，回归 `api_status_envelope_honors_status_config_defaults`）。`-u` 字段改为 `Option<UntrackedFiles>`（去掉 `default_value`，保留 `default_missing_value=all`），缺省经 config 回退 `normal`，恢复 CLI-对-config 优先级可判定。新增 `--no-branch`、`--no-show-stash`（`overrides_with` 负向对）。dirty-cache 扩展路径（`--scan`/`--cached`/`--check-dirty`）的 fresh 视图同样应用已解析默认：`showUntrackedFiles=no` 清空 untracked、`showStash` 填充 stash 计数、`relativePaths` 走共享渲染转换（cache 存显式路径，`normal`/`all` 在此模式渲染相同）。`commit --status` 模板经 `long_format: true` 固定长格式（Git 行为，不受 `status.short=true` 影响，回归 `commit_template_status_section_stays_long_with_status_short_config`）。回归：`compat_config_defaults_semantics` 新增 6 个 status 用例（untracked 三态全格式生效+CLI 覆盖、short/branch 仅塑形人类 short 且 porcelain 免疫、showStash 提示+负向覆盖、relativePaths=false 子目录仓库根路径+pathspec/:(top)/porcelain-v2 元数据/--exit-code 存活、fresh dirty-cache 应用三项默认、五键无效值 129 且无输出）+ `command_test` 的 commit 模板回归。
- 历史结论：当前文档应以这些提交之后的代码、测试和兼容矩阵为准；更早的迁移式文档只保留为背景，不再作为事实来源。

## 当前状态

- 公开状态：已公开；模块状态：已导出。
- 用户文档：`docs/commands/status.md`。
- Synopsis：`libra status [OPTIONS] [pathspec]...`。
- 公开参数/子命令包括：`<pathspec>...`（普通路径/目录前缀、默认通配符、`:(top)`、`:(exclude)`、`:(icase)`、`:(literal)`、`:(glob)`）、`-s, --short`、`--long`（显式选择默认长格式，no-op，与 `--short`/`--porcelain` 互斥）、`--porcelain [VERSION]`、`-b, --branch`、`--ahead-behind`、`--no-ahead-behind`、`--show-stash`、`--ignored`、`-u`/`--untracked-files [<MODE>]`（短/长形式；不带值即 `all`，短形式接受附加值 `-uno`/`-uall`/`-unormal`，经 `num_args=0..=1` + `default_missing_value=all`；默认 `normal`）、`--column`、`--no-column`、`-z`、`--find-renames [PERCENT]`、`--renames`、`--no-renames`、`--exit-code`。`--renames`/`--no-renames`（`overrides_with` 互斥）切换重命名检测：`--no-renames` 关闭（优先于 `--renames`/`--find-renames`），`--renames` 以默认（或 `--find-renames`）阈值开启。`--column`/`--no-column`（`overrides_with` 互斥）切换列对齐：`--no-column`（= `--column=never`）撤销先前的 `--column`（last-wins，读 `column` 布尔字段，`no_column` 不直接读取），status 默认非列式故单独为 no-op。
- P0-01 后，`collect_status_data` 通过 `src/command/unmerged.rs` 从 index stage 1/2/3 收集 unmerged entries，并从 untracked 集合剔除同一路径。short/porcelain v1 输出七类 XY；porcelain v2 输出 `u <XY> N... <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>`；默认长格式新增 `Unmerged paths:` 段，`--exit-code` 将 unmerged 视为 dirty。回归测试：`compat_conflict_status_diff`。
- P0-11 后，tracked symlink 按链接本身比较：link target bytes 或 mode 改变会进入 unstaged/staged 变更集合；dangling symlink 通过 `symlink_metadata` 视为存在路径，不会误报为 deleted。
- 2026-07-23（plan-20260714 §B.3.1）：unstaged rename 检测按 Git 默认收敛——unstaged 的"新"路径全部是未跟踪文件，只有 config-only Libra 扩展 `status.renameUntracked=true`（严格布尔、local→global→system 级联、非法值在任何输出前 fail-closed `LBR-CLI-002`）才允许它们成为 rename destination；默认关闭时已跟踪→未跟踪移动呈现 `D` + `??`，不产生 unstaged rename 记录。staged rename 检测不受影响。同片修复 RM/RD 组合记录：staged rename 记录的 Y 列现携带 NEW 路径的 worktree 状态（v1 此前缺 D、short 硬编码空格连 M 都丢、v2 缺 D 且 `RD` 时 `mW` 误报 100644——现为 000000）；endpoint 行被抑制时 Y 列是 destination worktree 状态在机器输出中的唯一信号。回归：`chain_rename_default_untracked_d_and_question`（含 v2 RD/mW=000000 与 `-z` RD 记录形状）、`rename_untracked_config_cascade`（含 fail-closed 时 stdout 为空）、`staged_rename_then_modify_emits_rm`（wave0 manifest 已登记），既有 unstaged rename 用例改为显式启用（`test_status_find_renames_detects_content_rename`、`test_status_find_renames_honors_threshold`、`test_status_renames_and_no_renames_toggle_detection`），并修正 `mv_case_only_rename_rekeys_index` 的过时断言（v0.19.4 起 rename 默认开启，case-only mv 折叠为 rename 记录）。同片实现 §B.3 **pathspec 逐端过滤**：rename 对仅当两端都命中 pathspec 才保留，old-only 降级为 deletion、new-only 降级为 addition，越界端点不再经 rename 记录泄露（此前 OR 保留同样影响默认 staged rename；回归 `pathspec_old_only_new_only_matrix`）。**已知限制（opt-in 路径，R0-3 治理）**：启用 `status.renameUntracked` 后 destination 候选取自展示层 untracked 列表——`-uno` 下无候选、折叠 untracked 目录隐藏其内文件；把配对与显示设置解耦的独立有界 probe 为 §B.3.1.1/§B.3.2 的 R0-3 交付（spec 测试 `rename_untracked_true_uno_probe_success`/`probe_skipped_when_untracked_disabled`），不影响默认（关闭）路径。
- 2026-07-23（plan-20260714 §B.5，R0-8a）：新增结构化警告 schema（`StatusWarning{code,message,source}`，snake_case 序列化钉死）与 §B.5 投递矩阵——human/short/porcelain 走 stderr + `record_warning()`（`--quiet` 不抑制），JSON 走 `data.warnings[]` 且零 stderr；rename 引擎 stats（`skipped_by_limit`/`exhaustive_discarded`）不再被丢弃。全部 5 个 `silent_exit(1)` 返回点前接入 9≻1 仲裁（`warning_exit` helper），修复 `--exit-code` 抢先导致顶层 exit-9 永不运行的缺陷。回归：`json_warnings_schema_snapshot`（完整 {code,message,source} 对象钉死）、`rename_limit_warning_exit_nine_over_dirty`（1001 文件触发 renameLimit；文本/quiet/scan 三路径 stderr warning + exit 9、JSON `data.warnings[]` 有码且 stderr 完全为空、无 on-warning 时 dirty 仍 1）、`similarity_budget_warning`（e2e：750×750 同 OID 内容仅 2 次对象读即可超 500k 比较预算——对象读取按 OID 缓存，读预算不阻断该路径）与单测 `rename_stats_map_to_structured_warnings`（stats→warning seam）。legacy `emit_warning`（dirty-cache 回退等，结构化映射属 R0-8b）已并入 9≻1 仲裁：`warning_exit` 同时检查全局 warning tracker，dirty exit 1 不再抢先任何 warning。剩余 R0-8b 已于同日随 v0.19.48 落地（见下条）。
- 2026-07-23（§B.5，R0-8b，v0.19.48）：dirty-cache 三个降级点改为结构化码——`dirty_cache_lock_stolen`（scan 抢占陈旧锁）、`dirty_cache_stale_fallback`（--cached 降级全量）、`dirty_cache_concurrent_invalidate`（读取期 index/HEAD 变化），source=`cache`；JSON cache 模式写入 `data.warnings[]` 且 stderr 为空，human 模式经共享投递走 stderr，9≻1 仲裁原生覆盖。回归：`json_cached_stale_fallback_warning`、`json_check_dirty_stale_fallback_warning`（pre-read stale 分支）、`json_check_dirty_concurrent_invalidate_warning`（mid-read 分支：LIBRA_TEST 门控的 `LIBRA_TEST_CACHE_READ_PAUSE_MS` 注入缝拓宽 read→re-verify 窗口，并发 add 确定性触发,生产环境该缝惰性）、`scan_lock_stolen_warning`（in-process 植入死 pid+过期时间戳的陈旧锁：JSON 码 + stderr 空、human stderr 行 + 9≻1）、`cache_stale_fallback_warning_exit_nine_over_dirty`;schema snapshot 钉住三码与 cache source;`dirty_cache_invalidated_by_index_write` 契约同步（JSON 零 stderr）;scan 的 stolen-lock 诊断由 wrapper 快照兜底：inner 任意失败点（append 前的指纹/收集/事务，或 append 后的 JSON emit 失败）都在错误路径 stderr 投递一次（JSON 仅错误路径破例——成功路径矩阵不变，诊断丢失比污染已失败的 envelope 更糟）；成功路径经 payload 恰好一次（human 用例断言 stderr 恰一行）。append 前/后错误注入与 render 失败注入的专属用例依赖 R0-8 故障注入基建（Codex review 完整轮次轨迹 R1–R16：R1 结构化三码/行为测试/快照缺口 4P1+1P2→修复；R2 scan 早退兜底部分关闭→wrapper；R3 append 过早清空→后移；R4 JSON 早退与 post-append emit 失败→clone 快照+无条件错误投递；R5 human render 双投递→is_silent 门控，本 deferral 首次登记并被接受；R6 println panic+quiet 缺口→可失败写+quiet 用例；R7 EPIPE 绕过定性为获准例外+quiet 断言收紧，deferral 复核维持；R8 render EPIPE 非 silent→13 处写闭包统一 P0-06 映射；R9 补 EPIPE 零 stderr 守卫用例；R10 组合不实/check-dirty println/mid-read e2e→注入缝落地并将该分支移出 deferral；R11 补 exit-0 断言；R12 两 fallback 分支后置投递；R13 JSON emit 直传+快照不含 rename 警告→canonical pending vec；R14 append 排空 vec→非破坏性 clone；R15/R16/R17 见下方独立存档条目。mid-read concurrent 分支原同列 deferral，R10 发现后以 LIBRA_TEST 注入缝落地 e2e 并移出清单；非 EPIPE stdout 失败在集成层无确定性注入点——EPIPE 已由 P0-06 设计排除为 silent——render/append 错误注入随故障注入基建整体交付）;quiet 成功路径恰一次已有用例;scan 摘要行改为可失败 write（bare println 会 panic 绕过兜底）。
- R0-8b review **R15** 存档：确认 R14 的非破坏性 clone 修复关闭（canonical vec 不再被 append 排空，post-append 任意非 silent 失败 wrapper 均持完整集）；同时要求本 deferral 具备可审计轮次记录——处置为轨迹首次入档。
- R0-8b review **R16** 存档：指出入档轨迹仅含 R5/R7/R10 节点、缺 R1 起完整逐轮记录——处置为补全 R1–R16 全部轮次摘要（见上条）。
- R0-8b review **R17** 存档：两项 P1——轨迹 R15/R16 未逐轮拆分（本三条独立条目即其处置）；EN/zh 用户文档 JSON stderr 契约"绝不"措辞与两处 fallback 实现不符——已校准为"成功运行干净、唯一例外为 envelope 非 EPIPE 写失败时刷 stderr"。
- P1-03 后，`status --porcelain=v1/v2 -z` 被固定为 NUL 记录语义：脚本应按 NUL 切分记录/字段，不能依赖换行或人读 rename 箭头。
- P1-05d 后，`status.showUntrackedFiles`/`status.short`/`status.branch`/`status.showStash`/`status.relativePaths` 按严格级联生效（五键前置校验、CLI 恒胜、porcelain 对格式类 config 免疫、`status.branch` 仅 short、`relativePaths=false` 仓库根路径）；`--no-branch`/`--no-show-stash` 为对应负向覆盖；`--long` 字段由此获得运行时消费者（压制 config short）。


## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容矩阵说明 | common Git status surface plus `-z` NUL-terminated output, `--find-renames`, `--renames`/`--no-renames`, `--column`/`--no-column`, and `--ahead-behind`/`--no-ahead-behind` supported | 按当前兼容矩阵保留；实现状态变化时同步 `_compatibility.md` 和测试证据。 |

## 维护要求

- 改进本命令前，必须先阅读并遵循 [docs/development/commands/_general.md](_general.md)；这是命令设计、实现、测试和文档同步的强制要求。
- 任何行为变更都要先核对实现源码，再同步 `COMPATIBILITY.md`、`docs/commands/<cmd>.md` 和相关测试。
- 新增 Git 兼容参数时必须明确 tier、错误码、JSON/机器输出契约和回归测试。
