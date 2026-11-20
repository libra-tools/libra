# `libra branch` Git 兼容与缺陷修复计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：Claude R8 与 Codex R8 双审 PASS（受审 SHA-256 前 12 位 `ea135d1e7ecd`；Codex 为 R7 六分区全文 PASS + R8 差异确认 PASS，Claude 为 R6 全文 PASS、R7 唯一 P1 修订后 R8 差异确认 PASS），计划可提交；全部任务 `pending`，尚未执行。计划文本提交门为 Claude（Subagent）与 Codex 对同一受审字节双审均 `VERDICT: PASS`（用户 2026-11-18 指令）。历史最近修订原记录为 `2026-11-17 12:27:28 UTC`，R46–R56 的实际发生时间待核实。本计划不在 v2.16 `code-only` 具名范围内，执行标准 `patch + 1` + `gh release create` 发布合同。
> 覆盖 Open Issues（21 个）：`#678`、`#679`、`#680`、`#681`、`#682`、`#684`、`#685`、`#687`、`#688`、`#689`、`#690`、`#691`、`#692`、`#693`、`#694`、`#695`、`#696`、`#699`、`#700`、`#701`、`#866`。

## 文档职责

- **网站交付路径（权威）:** 唯一生产网站 checkout 为 `/Volumes/Data/gitmono/libra-backend`（`cf` 分支；本机存在、为 `.libra` 仓库、无 `.git`）。依据：用户 2026-11-18 11:55:09 UTC 在协调会话中确认该路径为本批计划的网站交付检出。仓库 `AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`（本机不存在），属已知文档滞后，用户选择本批不改 `AGENTS.md`，本计划亦不修改该文件。每次网站写入前仍按「文档与网站验收命令」第 2 步核验该路径存在、仅含 `.libra`、当前分支为 `cf`；任一不满足即 STOP，禁止改用其它检出。

本文解决 `libra branch` 及关联通用修订解析（`@{upstream}` / `@{push}` / `@{-N}`）、`for-each-ref` 格式原子（`%(ahead-behind:...)` / detached HEAD）与 `fetch --all`（`skipFetchAll`）在 Git 兼容测试中的 21 项已知缺陷，目标是逐卡交付可独立验收、附带单元/集成测试与中英文/网站文档同步的修复；每张活动卡各自 `patch + 1` 独立发布（原颜色家族 `REL-BR-03` 及其 `BR-37`/`BR-38`/`BR-39` 已于 2026-11-18 撤回，`#680` 由独立卡 `BR-03` 按 issue 验收项交付，其余颜色轴登记 `DEFER-BR-03`）。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收。

### 适用范围

- `libra branch` CLI 参数解析（含 `action` / `query` 冲突组）、列表过滤/排序/格式化/着色、重命名/复制、上游追踪、远程追踪分支删除与错误退出码（`src/command/branch.rs`、`src/internal/branch.rs`、`src/internal/reflog.rs`）。
- 通用修订表达式与分支名解释中的 `@{upstream}` / `@{push}` / `@{-N}` 语法（`src/utils/util.rs`、`src/command/branch.rs`）。
- 共享格式化引擎的 `%(ahead-behind:...)` 原子与 detached HEAD 伪条目渲染（`src/command/for_each_ref.rs`）。
- `fetch --all` 对 `remote.<name>.skipFetchAll` 的支持（`src/command/fetch.rs`）。
- 对应测试（`tests/command/branch_test.rs`、`tests/command/rev_parse_test.rs`、`tests/command/for_each_ref_test.rs`、`tests/command/fetch_test.rs`，均编译在 `--test command_test`）、命令/网站文档与 `COMPATIBILITY.md`、`docs/error-codes.md`。

### 非目标

- 不重构 `branch diff` / `branch reset` 子命令或 `branch` 的 `--json` schema 既有字段（仅在新增输出/错误时保持兼容）。
- 不在本计划中修改 `checkout`、`switch`、`restore` 或 `grep` 命令本身（由同批并行计划承接）。
- 不引入新的 SQLite schema 迁移或修改 `GlobalConfig` / `SystemConfig` 表结构。

### 成功定义

- 21 个指定 GitHub Issue 对应的全部行为缺口在代码中修复，并有确定性 focused 测试（单元/集成）覆盖正常路径与失败路径。
- 每个修改用户可见行为的任务卡同步更新对应命令的 EN/zh-CN 文档、开发文档、网站文档及 `COMPATIBILITY.md`。
- 全部 35 张活动卡（`BR-01`..`BR-32`、`BR-34`、`BR-36`、`BR-40`；不含已撤回的 `BR-33`、`BR-35`、`BR-37`、`BR-38`、`BR-39`）达到 `Lifecycle=done` 且 `Acceptance=complete`，全部卡与收口全量门通过；`DEFER-BR-02`..`DEFER-BR-10` 所列内容不在本计划交付范围（涉及的 issue 在本计划完成后为部分闭环，见各 DEFER 行）。

## 使用规则

- 每次状态推进、快照更新与「修订历史」新增行都记录实际 `YYYY-MM-DD HH:MM:SS UTC` 时钟时间（v2.11）。
- 新建本计划或推进任意任务卡时，必须在同一变更中同步更新 [`plan-status.md`](plan-status.md)（本计划批量建稿期由协调者在统一提交中完成 `plan-status.md` 登记以避免并发写锁冲突；执行期每卡推进须同批更新 `plan-status.md`）。
- 所有测试摘要、计数、SHA-256、run ID、C/D 组门结果均写入本计划「实施证据汇总」或对应任务卡记录，不得新建 `evidence/`、`baseline/`、`scratch/` 等独立证据目录或文件（v2.12）。
- 终止链末端如存在纯文档记账提交，其远端 CI 观察由下一次 `plan-status.md` 例行同步补记（v2.14）。

## 事实基线

> 基线检查时间：2026-11-17 02:50:03 UTC，仓库基线 `main` @ `62abb622f0eb2626934633ada78061963ebf1f44`（`Cargo.toml` 版本 `0.30.207`）。所有行号必须在开工当天刷新。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `libra branch` 主实现位于 `src/command/branch.rs`（4187 行）；`BranchArgs` 定义参数解析（含 `ArgGroup("action").conflicts_with("query")`），`run_branch` 分发，`render_branch_output` 渲染输出 | `src/command/branch.rs:320-508,849-891,2600-2700,2827-3028` |
| 内部存储 | 分支引用与删除逻辑在 `src/internal/branch.rs`；reflog 存储与查询在 `src/internal/reflog.rs` | `src/internal/branch.rs:829-920`、`src/internal/reflog.rs` |
| 共享修订/格式/拉取 | `resolve_revision_expression_typed` 仅解析 `@{<digits>}`；`render_ref_format_lines` 设 detached `head_refname` 为 `None` 且无 `ahead-behind`；`fetch.rs` 未处理 `skipFetchAll` | `src/utils/util.rs:2635-2675`、`src/command/for_each_ref.rs:1391-1410,1580-1615`、`src/command/fetch.rs` |
| 用户命令 | `Commands::Branch(command::branch::BranchArgs)` 注册于 `src/cli.rs` | `src/cli.rs:622-623,2300` |
| 机器输出 | `BranchOutput` 支持 `--json` 输出；`BranchError` 映射为 `CliError` 与 `StableErrorCode` | `src/command/branch.rs:102-198,898-1005,2834-2836` |
| 错误码 | 错误码契约位于 `docs/error-codes.md` | `src/command/branch.rs:1040-1220`、`docs/error-codes.md` |
| 文档 | `docs/commands/{branch,for-each-ref,rev-parse,fetch}.md`（逐文件同步 EN 与 zh-CN）、`docs/development/commands/branch.md`、网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/{branch,for-each-ref,rev-parse,fetch}.en.md`、`COMPATIBILITY.md` | `docs/commands/branch.md`、`COMPATIBILITY.md:206` |
| 测试 | 集成测试模块 `branch_test`、`rev_parse_test`、`for_each_ref_test`、`fetch_test` 均注册于 `tests/command/mod.rs`，由唯一集成 target `command_test`（`tests/command_test.rs:10`）编译运行 | `tests/command/mod.rs:1011,1049,1126`、`tests/command_test.rs:10` |
| 外部参照 | Git `2.56.0` @ `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（`t/t3200-branch.sh`、`t/t3203-branch-output.sh`、`t/t3204-branch-checkout.sh`、`t/t5514-fetch-multiple.sh`）；核对日期 2026-11-17 | `gh issue view`（#678..#701, #866） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-BR-01 | `--column` 与 `-v` 未互斥；`render_branch_output` 静默忽略 `--column` | `#678` 未退出 128 报错 | `src/command/branch.rs:491-508,2913-2916` | `BR-01` |
| GAP-BR-02 | `new_branch` 属于 `action` 组与 `--list`（`query` 组）冲突被 clap 拒绝 | `#679` `--list <pattern>` 失败 | `src/command/branch.rs:320-365` | `BR-02` |
| GAP-BR-03 | 忽略 `color.branch.*` 配置 | `#680` 分支列表着色不兼容 | `src/command/branch.rs:2949-2967` | `BR-03` |
| GAP-BR-33 | 基线已用 `list_name_column_width` 对齐 `-v`/`-vv` 名称列 | 不再单独立项 | `src/command/branch.rs:2933-2937,3324-3339` | `BR-33` 撤回 |
| GAP-BR-04 | `resolve_upstream_spec` 未剥离 `refs/heads/`；自追踪警告/返回码路径需对齐 | `#681` `--set-upstream-to refs/heads/<b>` 失败 | `src/command/branch.rs:1400-1432,1495-1513` | `BR-04` |
| GAP-BR-05 | `unset_upstream_impl` 在无上游配置时仍返回 `Ok(())` | `#682` 缺少 `branch '<name>' has no upstream information` | `src/command/branch.rs:1675-1687` | `BR-05` |
| GAP-BR-06 | `resolve_revision_expression_typed` 拒绝 `@{upstream}`/`@{u}`，且 `branch` 缺少分支名解释 | `#684` `branch -f @{upstream}` 等失败 | `src/utils/util.rs:2649-2656`、`src/command/branch.rs` | `BR-06` |
| GAP-BR-25 | `util.rs` 未支持 `@{push}` 且 `for_each_ref::resolve_pushes` 未读取 `push.default` | `#684` `@{push}` 修订与分支名解释失败 | `src/command/for_each_ref.rs:250-262`、`src/utils/util.rs:2649` | `BR-40`, `BR-25` |
| GAP-BR-07 | 拒绝 `@{-1}` 及 `@{-1}@{upstream}` 的修订与分支名解释 | `#685` `--set-upstream-to X @{-1}` 等失败 | `src/utils/util.rs:2649-2675`、`src/command/branch.rs` | `BR-07` |
| GAP-BR-08 | `-m` 无参数由 clap `num_args = 1..=2` 拦截退出 129 | `#687` 未退出 128 且缺少 `branch name required` | `src/command/branch.rs:429-430,2247-2255` | `BR-08` |
| GAP-BR-09 | `rename_branch_impl` 未迁移源分支 reflog | `#688` 重命名后 `reflog exists` 失败 | `src/command/branch.rs:2282-2316` | `BR-09` |
| GAP-BR-10 | 创建/重命名/复制未检查 `o/o` 与 `o/p` 的 D/F ref 命名空间冲突 | `#689` 允许 D/F ref 碰撞 | `src/command/branch.rs:2274-2288`、`src/internal/branch.rs` | `BR-10` |
| GAP-BR-11 | `copy_branch_impl` 未复制 reflog 且仅复制 `remote`/`merge` | `#690` 丢失 reflog 与其他 `branch.<name>.*` 配置 | `src/command/branch.rs:2372-2384` | `BR-11` |
| GAP-BR-12 | `copy_branch_impl` 在 `!force` 时拒绝同分支复制 `m2 m2` | `#691` Git 允许 `-c m2 m2` 成功 | `src/command/branch.rs:2344-2350` | `BR-12` |
| GAP-BR-13 | `BranchArgs.force` 标记 `requires = "new_branch"` 导致 `-c -f` 被拒 | `#692` `branch -c -f` 无法执行强制复制 | `src/command/branch.rs:348-349,435-440` | `BR-13` |
| GAP-BR-14 | `--ignore-case` 缺少短标志 `short = 'i'` | `#693` `branch -i` 被拒绝 | `src/command/branch.rs:480-482` | `BR-14` |
| GAP-BR-15 | `BranchOutput::ShowCurrent` 在 detached HEAD 时打印 `HEAD detached at ...` | `#694` Git `--show-current` 在分离头保持静默 | `src/command/branch.rs:3012-3024` | `BR-15` |
| GAP-BR-16 | `merged` / `no_merged` 为 `Option<String>`，不支持重复传入 | `#695` 重复 `--merged` 报错退出 | `src/command/branch.rs:463-470,2459-2468` | `BR-16` |
| GAP-BR-17 | `BranchArgs` 未支持 `--forked [<pattern>]` 及其与 `--no-merged`/`--format` 组合 | `#696` `--forked` 被拒绝 | `src/command/branch.rs:331-508` | `BR-17` |
| GAP-BR-18 | `sort_branch_entries` 允许表未包含 `objecttype` | `#699` `--sort=objecttype` 被拒绝 | `src/command/branch.rs:3404-3416` | `BR-18` |
| GAP-BR-19 | `BranchArgs.sort` 为单值 `Option<String>`，不支持重复 `--sort` | `#700` 多键排序被拒绝 | `src/command/branch.rs:477-478,2552-2578` | `BR-19` |
| GAP-BR-20 | `for_each_ref` 原子表不支持 `%(ahead-behind:<committish>)` | `#700` `ahead-behind` 格式原子被拒绝 | `src/command/for_each_ref.rs:1580-1615` | `BR-20` |
| GAP-BR-21 | `render_ref_format_lines` 对 detached HEAD 置 `head_refname = None` 且 `branch.rs` 未注入伪条目 | `#701` `--format` 漏掉 detached HEAD 伪条目 | `src/command/branch.rs:2875-2899`、`src/command/for_each_ref.rs:1391-1410` | `BR-21` |
| GAP-BR-22 | `action` 与 `query` 组冲突且 `delete_branch_impl` 仅查本地分支，拒绝 `-r -d` | `#866` `branch -r -d` 被拒绝 | `src/command/branch.rs:320-330,2171-2233` | `BR-22` |
| GAP-BR-23 | `sort_branch_entries` 不支持 `ahead-behind:<rev>` 与 `-ahead-behind:<rev>` 排序键 | `#700` `t3203#33` 排序失败 | `src/command/branch.rs:3341,3404-3416` | `BR-23` |
| GAP-BR-24 | `fetch --all` 未读取并跳过 `remote.<name>.skipFetchAll=true` 的远端 | `#866` `t5514#9` `skipFetchAll` 验收项未覆盖 | `src/command/fetch.rs` | `BR-24` |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-long.md` | 长期路线参考 | 不触碰长期路线状态；本计划聚焦 21 个已归档缺陷收敛 |
| 同批并行计划（`clone` / `describe` / `merge-pull` / `global-init-infra` / `fetch-push` / `checkout-switch-restore` / `worktree-clean-reset-apply` / `plumbing-misc` / `grep` / `diff`） | 跨计划共享写集（只按卡 ID 引用，对方计划可能正被并行修订，开工当日以对方写集与 `plan-status.md` 为准） | `src/utils/util.rs`（本计划写者恰为 `BR-06`/`BR-07`/`BR-25`/`BR-26`/`BR-27`/`BR-28`/`BR-29`/`BR-30`/`BR-31`/`BR-32`/`BR-40`，与 describe `DEP-DESC-UTIL` 所列「`BR-06`、`BR-07`、`BR-25`..`BR-32`、`BR-40`」一致；其中写 `tests/command/rev_parse_test.rs` 与 rev-parse 文档的是除 `BR-28` 外的十张，`BR-28` 只写 `util.rs`；与 `clone`/`describe`/`merge-pull`/`global-init-infra`/`plumbing-misc` 共享，`DEP-BR-09`）；`src/command/fetch.rs`（`BR-24` 与 `fetch-push`/`clone` 共享，`DEP-BR-10`）；`src/command/branch.rs`（与 `checkout-switch-restore#CSR-10` 共享，`DEP-BR-11`）；`src/internal/branch.rs`（`BR-10`/`BR-34`/`BR-36`/`BR-22` 与 `worktree-clean-reset-apply#WC-10`、`plumbing-misc#PM-21a` 共享；对方 `DEP-PM-06` 与 `DEP-WC-01` 中拆分前编号 `PM-21` 即现 `PM-21a`，`DEP-BR-12`）；`COMPATIBILITY.md`/`docs/error-codes.md` 与网站生成产物（同批全部计划共享，`DEP-BR-08`、`DEP-BR-14`，按 global-init-infra `DEP-GII-03` 同口径串行）。`BR-03` 自 2026-11-18 起不再写 `src/cli.rs`/`src/utils/output.rs`（`DEP-BR-13` 撤回）；worktree-clean-reset-apply `DEP-WC-07` 仍把 `BR-03` 列为 `src/cli.rs` 写者，属对方已推送计划的过度登记，不形成环或冲突，执行期由协调者在 `plan-status.md` 同步。全部由协调者串行调度（G-10） |
| `plan-status.md` | 全仓计划单一视图 | 本计划建稿由协调者统一登记；执行期每卡推进同批更新 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 21 个 `branch` 兼容缺陷覆盖 `t3200`/`t3203`/`t3204`/`t5514` 等场景，值得按单轴切片逐一收敛 | 无 |
| 可行性 | 落点集中在 `branch.rs`、`internal/branch.rs`、`internal/reflog.rs`、`util.rs`、`for_each_ref.rs`、`fetch.rs` | 35 张活动卡（`BR-01`..`BR-32`、`BR-34`、`BR-36`、`BR-40`；`BR-33`、`BR-35`、`BR-37`、`BR-38`、`BR-39` 撤回） |
| 任务卡粒度 | `#700` 拆为 `BR-19`（重复 `--sort`）、`BR-20`（`%(ahead-behind)` 原子）、`BR-23`（`--sort=ahead-behind`）；`#866` 拆为 `BR-22`（`-r -d`）与 `BR-24`（`skipFetchAll`）；`#684` 拆为 `BR-06`/`BR-32`/`BR-31`/`BR-40`/`BR-25`/`BR-29`/`BR-27`/`BR-30`/`BR-26`/`BR-28`（`BR-40` 先交付 8 项过渡 fail-closed 守卫（7 项未实现配置 + `push.default=upstream`），`BR-25` 仅保留 upstream 与 remote 选择） | 全部卡满足 `S`/`M`、AC≤8、VER≤8；`EX-BR-01` 已取消。基线已对齐 `-v` 列宽，`BR-33` 不再实施；2026-11-18 撤回颜色家族（`BR-37`/`BR-38`/`BR-39`），`#680` 收窄为独立卡 `BR-03` |
| 依赖与顺序 | 按 Phase 0 -> Phase 1 -> Phase 2 严格对齐线性链 `BR-01 -> ... -> BR-28`，含后拆出的 `BR-34`、`BR-36`、`BR-40`（不含已撤回的 `BR-33`、`BR-35`、`BR-37`、`BR-38`、`BR-39`） | 无并发 I–I 冲突 |
| 完整性 | R1 补齐了 `--test command_test` 目标名、分支名解释接线、`skipFetchAll`、`--sort=ahead-behind` 与 `for-each-ref.en.md`/`rev-parse.md` 文档（2026-11-18 起不再改 `revision.md`：它是 `libra revision` 序号索引命令页，修订语法由 `rev-parse.md` 承载） | 无遗漏 |
| 安全性 | Reflog 迁移、分支删除与 D/F 碰撞校验 fail-closed | 在对应卡明确 |
| 功能正确性 | 精确对齐 Git 退出码（128 vs 129）、错误文案（含大小写）、D/F 碰撞与 detached HEAD 语义 | 已写入每卡 AC |
| 接口兼容 | `BranchArgs` 扩展保持向后兼容 | 已列写集 |
| 数据流与控制流 | 分支重命名/复制涉及 ref + metadata + reflog 顺序与事务边界 | 在 `BR-09`、`BR-11` 显式约束 |
| 性能与容量 | 列表排序/过滤与 `ahead-behind` 计算复用既有 helper，避免无界扫描 | 继承 GC-10 |
| 可靠性与容错 | `unwrap()`/`expect()` 禁止新增 | 继承 GC-11 |
| 可维护性 | 修订解析统一放 `util.rs`，格式原子/渲染统一放 `for_each_ref.rs`（GC-02） | 已分层 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 02:53:00 UTC | 初稿建档 | 基于 `agent_4_tasks.json` 21 个 `branch` issue 建立 `BR-01`..`BR-22` | N/A | 全文 |
| 2026-11-17 03:00:00 UTC | Claude & Codex R1 FAIL | 修复 `--test command_test` 目标名；拆出 `BR-23` 与 `BR-24`；补齐 `BR-06`/`07` 分支名解释与文档；补齐 `BR-17` 契约与 `DEP-BR-03..05`；对齐 Phase 0/1/2 | `BR-20` → `BR-20,23`；`BR-22` → `BR-22,24` | 实施顺序、依赖表、审计表、追溯表、里程碑、review log |
| 2026-11-17 03:08:00 UTC | Claude & Codex R2 FAIL | `BR-06` 拆出 `BR-25`（`@{push}`）；`Rollback mode` 改 `immutable-release` 并补 D 组门；`BR-17` 对齐 Git `--forked`；`BR-21` 补 `detached from`；登记 `DEFER-BR-02` | `BR-06` → `BR-06,25` | 实施顺序、任务卡、review log |
| 2026-11-17 03:15:00 UTC | Claude & Codex R3 FAIL | 补齐 `BR-25` 至审计/追溯/M3/默认覆盖；修正 `base.yml`；补 `immutable-release` 降级与兼容窗口；`BR-25` 下沉 `resolve_pushes` 至 `util.rs`；补 `DEP-BR-07`（#556） | N/A | 审计表、追溯表、依赖表、review log |
| 2026-11-17 03:23:00 UTC | Codex R4 FAIL | 补齐 `BR-25` 审计行与 `for_each_ref_test` 验证及自定义 refspec/错误透传 AC；修正 `BR-07` out-of-scope | N/A | 审计表、`BR-07`、`BR-25`、review log |
| 2026-11-17 03:26:00 UTC | Claude & Codex R5 FAIL | 统一全文 `24`→`25` 卡与 Phase 2 退出条件至 `BR-25`；同步 `BR-25` `Granularity` `AC=4/8; VER=3/8` 与 `for-each-ref` 文档影响；补齐风险表/收口清单与按时间正序的修订历史 | N/A | Phase 2、`BR-25`、风险表、收口清单、修订历史、实施证据汇总 |
| 2026-11-17 03:30:00 UTC | Codex R6 FAIL | 修正 `BR-25` 默认 `push.default=simple` 在无显式 `push.default`/`remote.<name>.push` 配置时结合 upstream 与 `origin` 解析 `@{push}` 的契约及测试；修正 `M1`..`M3` 回滚列为 `按 immutable-release 前滚修复` | N/A | `BR-25`、里程碑验收与回滚、review log |
| 2026-11-17 03:42:00 UTC | Claude & Codex R7 FAIL | 按 Git `remote.c` `branch_get_push_1` 重写 `BR-25`（`pushRemote`/`pushDefault`/`branch.remote`/`origin`、`remote.<r>.push` refspec、`mirror`、`nothing`/`matching`/`current`/`upstream`/`simple` 同名与三角失败、`@{p}`/`@{PUSH}`、`%(push)` 对齐及 `t1514` 具名测试）；修正 `ADR-BR-02` `revert` 残留措辞、G-03 谓词计数口径说明与 `实施证据汇总` 时间戳 | N/A | `BR-25`、`ADR-BR-02`、评审结论、审计表、实施证据汇总、review log |
| 2026-11-17 03:46:00 UTC | Claude & Codex R8 FAIL | 移除非 Git 语法 `@{p}`（改为 `@{PUSH}`/`@{pUSh}`/`@{PuSH}`）；修正 `simple` 跨远端三角失败语义；限定 detached HEAD 失败仅限从 `HEAD` 推断时（显式 `<branch>@{push}` 正常解析）；将 `BR-25` 拆分为 `BR-25`（remote 选择 + `simple`/`upstream` + detached HEAD）与 `BR-26`（`nothing`/`matching`/`current` + `remote.<r>.push`/`mirror`/`tracking_for_push_dest` + `%(push)`）各 `7/8` 原子 AC | `BR-25` → `BR-25,26` | 全文 |
| 2026-11-17 03:53:00 UTC | Codex R9 FAIL | `BR-25` 增加第 8 条原子 AC（`AC=8/8`）：`BR-26` 落地前若存在 `remote.<r>.push` 或 `remote.<r>.mirror=true` 必须 fail-closed 报错而非回落 `push.default`，同步更新测试与兼容说明；修正 `评审结论` 依赖链至 `BR-26` | N/A | `评审结论`、审计表、`BR-25`、实施证据汇总、review log |
| 2026-11-17 03:57:00 UTC | Codex R10 FAIL | 将「成功定义」中的 `25` 张任务卡统一同步为 `26` 张任务卡 | N/A | 成功定义、实施证据汇总、review log |
| 2026-11-17 04:06:00 UTC | Claude & Codex R11 FAIL | 将 `BR-25` 拆为 `BR-25`（`7/8`）+ `BR-27`（`5/8`）、将 `BR-26` 拆为 `BR-26`（`7/8`）+ `BR-28`（`3/8`），全部原子 AC≤8；`BR-26` 补齐 `branch`/`revision` 中英文与网站文档并清除过渡限制；`BR-06` 补齐远程 `@{upstream}` 拒绝（`t3204#4`）与标题 `1/5`；`BR-07` 明确 `branch.upstream-other.merge`（`t3204#16`）；`BR-21` 对齐 `for-each-ref` 文档；澄清 `DEFER-BR-02` 不延后 `#866` `t5514#9` `skipFetchAll` 行为验收（由 `BR-24` 完整覆盖）；修复修订历史表空行 | `BR-25`→`25,27`；`BR-26`→`26,28` | 全文 |
| 2026-11-17 04:19:00 UTC | Codex R12 FAIL（Claude R12 `ECONNRESET`） | `BR-25` 增加第 8 条原子 AC（`AC=8/8`）：在 `BR-27`/`BR-26` 落地前对未显式配置 `push.default`（默认 `simple`）及 `nothing`/`matching`/`current` 显式 fail-closed 报错并补齐验证与过渡边界说明；展开「发布窗口顺序」为 `BR-01` → … → `BR-24` → `BR-25` → `BR-27` → `BR-26` → `BR-28` | N/A | 发布窗口、审计表、`BR-25`、实施证据汇总、review log |
| 2026-11-17 04:24:00 UTC | Codex R13 FAIL（Claude R13 `ECONNRESET`） | 将 `BR-25` 拆出 `BR-29`（`<branch>@{push}`、大小写变体 `@{PUSH}`/`@{pUSh}`/`@{PuSH}` 与 `branch` 分支名解释，`3/8`），使 `BR-25` 的 8 条 AC 严格保持每条 1 个独立原子谓词（`8/8`，逐一对应 `unset`/`nothing`/`matching`/`current`/`remote.<r>.push`/`mirror` 过渡 fail-closed + 当前分支 `@{push}` + remote 选择）；同步 `#684` 部分 `1/6`..`6/6`、`29` 卡计数、实施顺序与发布窗口至 `BR-25 → BR-29 → BR-27` | `BR-25` → `BR-25,29` | 全文 |
| 2026-11-17 04:30:00 UTC | Codex R14 FAIL | 将 `BR-25` AC1 收敛为单谓词（仅解析目标提交），补齐显式 `push.default=simple` 过渡 fail-closed 原子 AC 及测试断言，按 `plan-template.md` v2.16 门族型验收白名单为 `BR-25` 7 项机械配置守卫登记 `EX-BR-01`（`AC=9/8@EX-BR-01`）与「判据规范（非计数正文）」块；在计划级 `T-1` 默认覆盖表补齐 `BR-29` | N/A | 默认覆盖、waiver 表、审计表、`BR-25`、实施证据汇总、review log |
| 2026-11-17 04:39:00 UTC | Codex R15 FAIL & Claude R14 FAIL | 将 `tracking_for_push_dest`（fetch refspec 追踪映射与 `has no local tracking branch` 报错）自 `BR-26` 前移至 `BR-27` 并独立区分缺失 upstream、缺失本地追踪分支与映射不一致报错（`8/8`），清除 `BR-26` 对已前移项；`BR-29` 将 `<branch>@{push}` ref/commit 拆为 2 条原子 AC 并以 CLI 可观测分支结果表述 AC3（`4/8`）；`BR-17` 明确重复 `--forked` 并集语义与混用断言（`4/8`）；`BR-06` 补齐 `@{UPSTREAM}`/`@{U}` 大小写不敏感（`5/8`）；`BR-27`/`BR-26` 写集收敛至 `util.rs`；同步 `评审结论` 与 `ADR-BR-02` 至 29 卡与 `EX-BR-01` | N/A | 评审结论、`ADR-BR-02`、审计表、`BR-06`、`BR-17`、`BR-29`、`BR-27`、`BR-26`、追溯表、实施证据汇总、review log |
| 2026-11-17 04:48:00 UTC | Codex R16 FAIL（Claude R16 `ECONNRESET`） | `BR-06` 将 detached HEAD 报错限定为从 `HEAD` 推断的隐式 `@{upstream}`/`@{u}` 并新增 detached HEAD 下显式 `<branch>@{upstream}` 成功判据（`6/8`）；`BR-09` 与 `BR-11` 各自新增中间变更失败的原子回滚/补偿 AC（`4/8`）及故障注入回归测试（`VER=3/8`）；登记 `DEP-BR-08` 协调跨计划共享 `COMPATIBILITY.md`/`docs/error-codes.md` 串行合入窗口 | N/A | 依赖表、审计表、`BR-01`、`BR-06`、`BR-09`、`BR-11`、实施证据汇总、review log |
| 2026-11-17 04:53:00 UTC | Codex R17 FAIL | `BR-11` 原子回滚 AC4 与 `copy_partial_failure_rollback` 补齐目标分支 `metadata`（`MetadataKv`，`branch.rs:2392`）恢复断言；明确 `DEP-BR-08` 为 `BR-01..BR-28` 计划级串行编辑锁（未获锁前阻塞编辑 `COMPATIBILITY.md`/`docs/error-codes.md` 共享文件，由协调者合入时统一登记 `plan-status.md`，并同步接入 `BR-05`/`BR-08`） | N/A | 依赖表、审计表、`BR-05`、`BR-08`、`BR-11`、实施证据汇总、review log |
| 2026-11-17 05:03:00 UTC | Codex R18 FAIL | 将 `DEP-BR-08` 覆盖范围统一至全部 29 卡（`BR-01..BR-29`，含 `BR-29`）并改为逐卡获取/释放串行编辑锁；将当时全文网站文档路径统一为 `/Volumes/Sea/libra-backend`（`cf` 分支，按 ER-06a 不可用则 STOP；此为历史路径，后续用户指令已替换） | N/A | 全文 |
| 2026-11-17 05:12:00 UTC | Claude & Codex R19 FAIL | 将 `DEP-BR-08` 同步接入全部 29 张任务卡的 `Dependencies`、`Granularity` 与审计表；展开所有卡 `Docs and compatibility impact` 的逐文件清单与 ER-06/ER-06a 逐文件验收门；`EX-BR-01` 审批人具名为 `Eli（仓库维护者 / Plan Owner）` | N/A | waiver 表、审计表、全部 29 张卡、实施证据汇总、review log |
| 2026-11-17 05:15:00 UTC | Claude & Codex R20 FAIL | 将 `BR-06` 拆出 `BR-31`（detached HEAD `@{upstream}` 规则，`3/8`），`BR-06` 细化为 6 条独立单谓词 AC（`6/8`）；将 `BR-27` 拆出 `BR-30`（detached HEAD `@{push}` 规则，`2/8`），`BR-27` 将未配置 `push.default` 与显式 `push.default=simple` 拆为两条独立 AC（`7/8`）；展开 `BR-20`/`BR-21` 完整逐文件 `Docs and compatibility impact`（含 `BR-20` 补入 `docs/development/commands/for-each-ref.md`）；清理 `BR-06`/`07`/`25`/`29`/`27`/`26`/`28` 重复文档碎片并统一 `split-from` 谱系；修正 R19 日志 `P3` → `P2`；全量同步至 31 卡 | `BR-06`→`06,31`；`BR-27`→`27,30` | 全文 |
| 2026-11-17 05:22:00 UTC | Claude & Codex R21 FAIL | 将 `BR-06` 拆出 `BR-32`（`@{u}` 与大小写变体 `@{UPSTREAM}`/`@{U}`，`6/8`），使 `BR-06`（`5/8`）与 `BR-32` 每条 AC 严格单谓词；全部 32 张卡补齐同卡 ER-06/ER-06a 文档逐文件交付验收门（不计入 G-03 卡特有 AC 分子）；将 `DEP-BR-08` 覆盖范围、实施顺序、`发布窗口顺序`、审计表、追溯表（补齐 `BR-31`/`BR-32`/`BR-30` 并修正 `BR-27` 测试名 `push_simple_and_tracking`）、`评审结论`、`M2`/`M3` 与 `#684` 部分编号（`1/9`..`9/9`）全量同步至 32 卡；校准 `BR-01`/`14`/`07`/`10` AC 计数、清理 `BR-28` 重复文档碎片、对齐 `BR-25`/`29`/`27`/`26`/`28` 过渡文档清除说明及 `status.rs:883` 锚点 | `BR-06`→`06,32,31` | 全文 |
| 2026-11-17 05:26:00 UTC | Codex R22 FAIL | 将 `BR-32` AC6（`branch -f @{UPSTREAM}` 与 `@{U}`）拆为两条独立单谓词 AC（`7/8`）；将 `BR-31` detached HEAD 隐式 `@{upstream}`/`@{u}` 失败与显式 `<branch>@{upstream}`/`<branch>@{u}` 成功拆为 4 条独立单谓词 AC（共 `5/8`），同步更新 `Granularity` 与审计表 | N/A | 审计表、`BR-32`、`BR-31`、实施证据汇总、review log |
| 2026-11-17 05:30:00 UTC | Claude R22 FAIL | 补齐跨计划共享源码写集依赖 `DEP-BR-09`（`util.rs`）、`DEP-BR-10`（`fetch.rs`）、`DEP-BR-11`（`branch.rs`）、`DEP-BR-12`（`internal/branch.rs`）并接入全部对应任务卡与审计表、更新 `与其它计划的关系`（含 10 张 `util.rs` 卡及 `diff`）；将网站文档路径统一改为本地已挂载的 `/Volumes/Data/gitmono/libra-backend`（`cf` 分支）；拆细 `BR-29` AC4（`-f` 与 `--set-upstream-to`，`5/8`）、补齐 `BR-21` `%(refname)` AC（`3/8`）、移除 `BR-30` 重复过渡说明并修正 `EX-BR-01` 评审轮次为 `R14` | N/A | 全文 |
| 2026-11-17 05:35:00 UTC | Codex R24 FAIL（Claude R24 `ECONNRESET`） | 将 `BR-21` 拆细为独立单谓词 AC；在 `DEP-BR-02` 明确本地与执行机挂载授权路径 `/Volumes/Sea/libra-backend`（`cf` 分支）的优先核对规则（此为历史路径，后续用户指令已替换）（无有效 `cf` 仓库则 STOP） | N/A | `DEP-BR-02`、审计表、`BR-21`、实施证据汇总、review log |
| 2026-11-17 05:42:00 UTC | Codex R25 FAIL（Claude R25 `ECONNRESET`） | 将 `BR-21` AC1/AC2 进一步拆细为 8 条严格单谓词 AC（分别独立验证 detached-at 的 `%(HEAD)=*`/`%(refname)`/`%(refname:short)`、detached-from 的 `%(refname)`/`%(refname:short)`、列表顺序、非 detached 与 `-r` 排除，同步 `AC=8/8` 至 `Granularity` 与审计表）；移除 `Codex review log` 中并无独立评审记录的 `R23` 占位行（R23 修订已并入 R24 统一复审） | N/A | 审计表、`BR-21`、修订历史、实施证据汇总、review log |
| 2026-11-17 07:46:33 UTC | Codex R26 FAIL（Claude R26 `ECONNRESET`） | 网站写集与 `DEP-BR-02` 固定为 `/Volumes/Data/gitmono/libra-backend`（缺失则 STOP）；`BR-27`/`BR-30`/`BR-26` 补入 `branch` 中英文、开发文档与网站 `branch.en.md` | N/A | `DEP-BR-02`、`BR-26`/`27`/`30`、Review log |
| 2026-11-17 08:28:29 UTC | Codex R27/R29 FAIL | `BR-03` 级联与 `--color` 各收成单谓词并补 `EX-BR-02` 逐门清单与追溯测试名；`BR-09` reflog AC 拆为 5/8 | N/A | waiver、`BR-03`、`BR-09`、追溯表、Review log |
| 2026-11-17 09:06:33 UTC | Codex R30 FAIL | `BR-29` 增补分支名槽 `branch -f @{push} <commit>` 与远程追踪目标拒绝/不改写，大小写收成单谓词（`AC=8/8`）；`BR-10` 拆成 8 条单谓词；`BR-20`/`BR-21` 落点按 `src/command/` 计为 `landing=1`、`prod-files=2`；评审结论补 `EX-BR-02` | N/A | `BR-10`、`BR-20`、`BR-21`、`BR-29`、审计表、评审结论、Review log |
| 2026-11-17 09:36:28 UTC | Codex R31 FAIL | 撤回 `EX-BR-02`。`BR-03` 只保留着色轴（`AC=8/8`，`VER=8/8`）。列对齐拆为 `BR-33`（`AC=2/8`），插入 `BR-03` 与 `BR-04` 之间，并同步实施顺序、发布窗口、Phase 0、`DEP-BR-08`、审计表、追溯表、M1/M3 | `BR-03` → `BR-03, BR-33` | waiver、`BR-03`、`BR-33`、`BR-04`、审计表、追溯表、里程碑、Review log |
| 2026-11-17 09:51:20 UTC | Codex R32 FAIL | 基线已对齐 `-v` 列宽，撤回 `BR-33` 实施卡；Phase 2 退出条件明确 `BR-33` 不是完成门；活动卡回到 32 张 | `BR-33` 撤回 | 实施顺序、Phase 0/2、审计表、`BR-04`、里程碑、Review log |
| 2026-11-17 09:57:57 UTC | Codex R33 FAIL | `BR-10` 只留 4 条退出非零（`AC=4/8`）；错误点名、`m m/m` 与复制冲突拆为 `BR-34`（`AC=7/8`）；同步 Phase 1 与 M2 | `BR-10` → `BR-10, BR-34` | 实施顺序、审计表、`BR-10`、`BR-34`、`BR-11`、里程碑、Review log |
| 2026-11-17 10:09:42 UTC | Codex R34 FAIL | `BR-10`/`BR-34` 改为 `REL-BR-10` 家族子卡，由 `BR-35` 统一发布；`BR-11` 依赖改为 `BR-35`；Phase 2 纳入 `BR-34`/`BR-35` | `BR-10` → `BR-10, BR-34, BR-35` | 发布分组、`BR-10`、`BR-34`、`BR-35`、`BR-11`、Phase 2、依赖表 |
| 2026-11-17 10:43:14 UTC | Codex R39 FAIL | 撤回 `REL-BR-10` 与 `BR-35`。`BR-10` 只交付创建冲突的退出码和错误点名；`BR-34` 只交付重命名冲突的退出码、错误点名和 `m m/m` 源排除；新增 `BR-36` 交付复制冲突的退出码和错误点名。三张卡各自 `patch + 1` | `BR-34` → `BR-34, BR-36`；`BR-35` 撤回 | 发布分组、实施顺序、审计表、`BR-10`、`BR-34`、`BR-36`、`BR-11`、里程碑、风险表 |
| 2026-11-17 10:52:35 UTC | Codex R40 FAIL | `BR-36` 补上已有 `o`、目标 `o/p` 的反向冲突：`-c` 与 `-C` 都要退出非零并点名 `o`。测试矩阵列出创建、重命名、复制三条指定测试 | N/A | `BR-36`、测试矩阵、审计表、Review log |
| 2026-11-17 11:14:03 UTC | Codex R41 FAIL | `BR-03` 的非 auto 级联保持原样。`auto` 的终端与重定向拆为 `BR-37` | `BR-03` → `BR-03, BR-37` | `BR-03`、`BR-37`、实施顺序、Phase 0、M1、审计表、追溯表 |
| 2026-11-17 11:19:54 UTC | Codex R42 FAIL | `DEP-BR-08` 与 Phase 2 退出条件纳入 `BR-37`。评审结论线性链和 `DEFER-BR-01` 同步 | N/A | `DEP-BR-08`、Phase 2、评审结论、`DEFER-BR-01` |
| 2026-11-17 12:01:37 UTC | Codex R45 FAIL | `BR-03` 补全 `OutputConfig` 全局禁色优先级、显式 `--color=auto` argv 传递及普通/unborn/detached/column 路径；`BR-37` 补四渲染路径与槽作用域；新增 `DEP-BR-13` 并同步粒度；按用户指令改为 Codex 单审提交门 | N/A | `DEP-BR-13`、`BR-03`、`BR-37`、审计表、追溯表、M0、完成判据、Review log |
| 2026-11-17 12:27:28 UTC（原记录时间；以下同时间各轮实际发生时间与登记性质待核实） | Codex R46 FAIL | M0 改为 Codex 单审；新增 `BR-38` 承接普通/unborn/detached/column 的默认样式与自定义槽矩阵，避免把路径合并谓词塞入 `BR-03`/`BR-37`；按 Git 默认样式修正普通本地非当前分支无 ANSI 的断言，并同步 36 卡 DAG/粒度/追溯 | `BR-37` → `BR-37,BR-38` | 头部、成功定义、DAG、`DEP-BR-08`、Phase 0/2、审计表、`BR-03`/`BR-37`/`BR-38`/`BR-04`、追溯表、M0/M1/M3、Review log |
| 2026-11-17 12:27:28 UTC | Codex R47 FAIL | 新增 `REL-BR-03` 与发布点 `BR-39`，将 `BR-03`/`BR-37`/`BR-38` 改为家族子卡，确保颜色模式、槽和四类入口首次发布前共同验收；同步实施证据最新修订时间 | `BR-38` → `BR-38,BR-39` | 发布分组、DAG、默认覆盖、审计表、`BR-03`/`BR-37`/`BR-38`/`BR-39`/`BR-04`、追溯表、M1/M3、实施证据、Review log |
| 2026-11-17 12:27:28 UTC | Codex R48 FAIL/P2 | `ADR-BR-02` 与完成判据改为独立卡逐卡发布、`REL-BR-03` 子卡继承 `BR-39` C/D；补齐家族窗口禁止插入项及 Libra/网站发布前逆序恢复和发布后前滚规则 | N/A | `ADR-BR-02`、发布分组、完成判据、Review log |
| 2026-11-17 12:27:28 UTC | Codex R49-A/B FAIL | 全局发布条款改为仅独立卡与 `BR-39` 自行 C/D；`DEP-BR-08` 改家族级锁贯穿四卡并在统一推送或回滚后释放；发布禁令仅限子卡阶段，允许 `BR-39` 聚合后统一发布；跨计划顺序统一为 `BR-10`/`BR-22` → `WC-10` → `PM-21` | N/A | 执行检查、`DEP-BR-08`、`DEP-BR-12`、发布分组、Review log |
| 2026-11-17 12:27:28 UTC | Codex R50-A/B FAIL/P2 | `BR-03` 增加 `--no-color` 覆盖与共享 OutputConfig/非 branch 回归；`BR-25` 增加 upstream 跨 remote 拒绝；`BR-22` 增加中途失败回滚及 AC/VER；性能数值改为非门禁观测目标 | N/A | `BR-03`/`BR-22`/`BR-25`、审计表、追溯表、性能摘要、Review log |
| 2026-11-17 12:27:28 UTC | Codex R51-A/B FAIL/P2 | `BR-09` 增加 `-M` 覆盖目标失败恢复，`BR-11` 增加 copy 记录断言；将 7 项过渡守卫拆至 `BR-40` 并置于 `BR-25` 前，`BR-25` 仅保留同 remote 成功、跨 remote 失败与 remote 选择，取消 `EX-BR-01` | `BR-25` → `BR-40,BR-25` | 头部、成功定义、DAG、默认覆盖、审计表、`BR-40`/`BR-25`、追溯表、M3、waiver、Review log |
| 2026-11-17 12:27:28 UTC | Codex R52-A/B FAIL/P2 | `DEP-BR-09` 纳入 `BR-40`；`BR-09` 交付 `-M` 成功路径及成功/失败双测试；`BR-40` 明确交付最小 `@{push}` 入口与 7 个可区分过渡拒绝；文档清除责任改指 `BR-40` | N/A | `DEP-BR-06`、`DEP-BR-09`、`BR-09`、`BR-40`、`BR-27`/`BR-26` 文档、审计表、追溯表、Review log |
| 2026-11-17 12:27:28 UTC（原记录时间；各轮实际时间待核实） | Codex R53-A/B FAIL/P2 | 普通 `-m` 恢复范围补入源/目标 `branch.<name>.*` config 快照与逐项断言；风险表纳入 `BR-40`，`BR-25` 明确扩展现有入口；R53-B 另记风险表与既有解析入口修订，实际修订时间待核实 | N/A | `BR-09`、`BR-25`、风险表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R54-A/B FAIL | 普通 `-m` 成功路径补全量配置迁移与清理；`BR-40` 增加 `push.default=upstream` 过渡拒绝，`BR-25` 同卡移除该限制 | N/A | `BR-09`、`BR-40`/`BR-25`、审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R55-A/B 评审 | R55-B 分区 PASS，无 P0/P1/P2。R55-A P1：`-M` 成功配置状态不明确，已明确源配置迁移、覆盖前目标配置与旧名称配置清理及逐项断言 | N/A | `BR-09`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R56-A PASS | R56-A 覆盖第 1–1000 行、完成判据与 `DEP-PM-06`，普通 `-m`/`-M` 配置迁移与恢复、`DEP-BR-09`、`REL-BR-03` 均无 P0/P1/P2；与 R55-B 分区 PASS 共同构成计划 Codex 单审通过 | N/A | 头部、Review log |
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（用户最新指令指定；仓库 `AGENTS.md` 的 `/Volumes/Sea/libra-backend` 已被取代且本机不存在），并在 `DEP-BR-02` 写明权威依据与 STOP 条件；修正 `BR-04` 自追踪拆分为 4 项独立验收并同步粒度与审计表（`AC=4/8`）、补正向 `remote`/`merge` 与两个自追踪入口断言、追溯行补入单元测试；`R53-B` 合并进 `R53` 记录并标注时间待核实；评审日志规则明确无 P0/P1 时填 `无 P0/P1` 且仅有 P2 仍记 `FAIL`，历史字节限定移入 Scope/Evidence；`M3` 补 `BR-40`；头部与实施证据标注旧 PASS 仅适用历史字节 | N/A | 头部、依赖表、文档职责、`BR-04`、审计表、追溯表、里程碑、修订历史、Review log、实施证据汇总 |
| 2026-11-18 09:43:54 UTC（原行误记为 `2026-11-18 10:35:00 UTC`，晚于本文件该批修订的最后写入时刻（文件 mtime `2026-11-18 09:43:54 UTC`），实际写入时间不晚于该时刻、具体时刻待核实；R1 按 mtime 更正） | Codex 复审 FAIL 修复（验证契约与门禁） | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 09:19:00 UTC | Codex 分区复审 FAIL 修复 | `BR-05` 拆为无上游（exit 128 与提示）与有上游（exit 0、清除 `remote`/`merge`、其余配置不变）两条具名测试；`BR-08` 拆为 0 参数 exit 128 与 `-m <new>`/`-m <old> <new>` 重命名及超 2 参数 exit 129 两条具名测试；新增「文档与网站验收命令」块（逐文件 SHA 比对、`sync-libra-command-docs.py --write/--check --only`、`pnpm typecheck`/`build`、网站最终字节独立 Claude Code `VERDICT: PASS`、提交树核对与 `cf` 推送部署），并写明计划 Codex 单审与网站 Claude 门互不替代；`BR-01`/`BR-02` 补逐项断言（互斥与单独使用、`--list`/`-r`/`-a`/零匹配），`BR-03` 补 `--machine` 与显式 `--color=always` 组合无 ANSI 断言 | N/A | 执行检查必备需求、`BR-01`/`BR-02`/`BR-03`/`BR-05`/`BR-08`、完成判据 |
| 2026-11-18 09:43:54 UTC（原行误记为 `2026-11-18 09:52:00 UTC`，晚于本文件该批修订的最后写入时刻（文件 mtime `2026-11-18 09:43:54 UTC`），实际写入时间不晚于该时刻、具体时刻待核实；R1 按 mtime 更正） | Codex 七分区复审 FAIL 修复 | 新增「验证执行协议」（精确 `-E` 过滤、先行 `cargo nextest list` 按模块级规则记录匹配数、记录 exit code 与四计数、逐条 AC 断言、模块级回归标注共享回归组），并在全部 94 条 Verification 命令行内联引用该协议；「文档与网站验收命令」补家族子卡只做本地门与本地提交、唯一发布点统一推送，并给出 hash/暂存/提交树/推送/部署核验的具体命令与 STOP 条件；`BR-01` 补四种 `--column`/`-v` 组合断言、`BR-02` 补多模式/`-l` 别名/带 `start_point` 创建、`BR-03` 补 `--machine` 与显式 `--color=always` 组合、`BR-37` 补槽边界与四条渲染路径双禁色模式、`BR-39` 补 20 个具体测试名逐项核对与零跳过要求、`BR-24` 补 skipFetchAll 双场景、`BR-26` 补 `custom_refspec_no_match_fail` 负例、`BR-29` 补大小写变体与 set-upstream 配置断言、`BR-22` 改精确等值过滤消除前缀冲突并补完整引用名输入；完成判据补字面 `VERDICT: PASS` 与最终树 T-2 重跑 | N/A | 执行检查必备需求、`BR-01`/`BR-02`/`BR-03`/`BR-22`/`BR-24`/`BR-26`/`BR-29`/`BR-37`/`BR-39`、完成判据、追溯表、修订历史 |
| 2026-11-18 09:43:54 UTC（原行误记为 `2026-11-18 10:05:00 UTC`，晚于本文件该批修订的最后写入时刻（文件 mtime `2026-11-18 09:43:54 UTC`），实际写入时间不晚于该时刻、具体时刻待核实；R1 按 mtime 更正） | Codex 二轮七分区复审 FAIL 修复 | 为全部 38 张活动卡（含发布点 `BR-39`）新增「AC → 断言映射」，逐条把 AC 绑定到具体测试名与其可观测断言（退出码 / stdout / stderr / 前后状态），消除「只列测试名」缺口；`BR-33`/`BR-35` 为已撤回卡不适用 | N/A | 全部 38 张活动卡、修订历史 |
| 2026-11-18 17:49:51 UTC | R1 预审清扫（用户 2026-11-18 双审指令；受审前字节 SHA-256 `94af07550601…`） | ① 计划文本提交门改为 Claude（Subagent）与 Codex 对同一字节双审均 PASS（头部、执行检查、Phase 0、M0、完成判据、Review log 口径「无 P0/P1 即 PASS」），执行期 ER-05 与网站 Claude Code 门不变；② 网站路径依据改为用户 2026-11-18 11:55:09 UTC 确认，`AGENTS.md:135` 登记为已知文档滞后、取消额外前置；③ 三行晚于文件 mtime 的修订时间（10:35:00/09:52:00/10:05:00）按 mtime 更正；④ 重写「验证执行协议」（nextest 过滤排除计入 `S`，不再要求跳过 = 0；共享回归组不计入 VER；夹具/快照/故障注入/NOESC 约定）与「文档与网站验收命令」7 步（以页面检查取代无法绑定提交的 `wrangler` 部署元数据核验）；⑤ 按范围控制撤回颜色家族 `REL-BR-03`（`BR-37`/`BR-38`/`BR-39`），`BR-03` 收窄为 `#680` 验收项（三槽 + `-v -a` 布局，按 pinned Git 实测字节；本地非当前分支有复位序列 `ESC[m`，原「无 ANSI」与 Git 不符），`DEP-BR-13` 撤回、新增 `DEP-BR-14`（网站交付窗口，按 `DEP-GII-03` 口径）；⑥ 全部活动卡「AC → 断言映射」由机械占位改写为可观测判据，`Verification` 只列命令与覆盖 AC；⑦ 以 pinned Git 实测修正与 Git 不符的 AC：`BR-25` AC2（`upstream` 模式不校验 push remote）、`BR-13` AC2（`-f`/`--list -f` 列出分支而非拒绝）、`BR-08` AC2（超过 2 参数改 `DEFER-BR-05`）、`BR-12`（同名复制追加 reflog）、`BR-22`（不接受 `refs/remotes/` 全名）；⑧ `BR-09` 的 `-M` 判据移入 `DEFER-BR-04`（`#675`），`BR-11` 的 `-C` 覆盖语义移入 `DEFER-BR-06`；⑨ `BR-21` 删除不存在的 `br_21_ac2` 用例；⑩ 跨计划：`PM-21`→`PM-21a`、`DEP-BR-08`/`09` 接 `DEP-GII-03`、describe `DEP-DESC-UTIL` 写者核对一致；⑪ 收口全量门绑定 `docs/development/plan/` 以外的最终代码树 | `BR-37`/`BR-38`/`BR-39` → 撤回（`#680` 由 `BR-03` 承接） | 头部、文档职责、关系表、ADR-BR-02、执行检查必备需求、实施顺序、依赖表、发布分组、Phase、默认覆盖、审计表、全部活动卡、测试矩阵、追溯表、里程碑、风险、Review log、实施证据、延后项、完成判据 |
| 2026-11-18 18:00:07 UTC | 双审 R1：Claude FAIL（6×P1、1×P2）、Codex FAIL（A–F 六分区共 27 条 P1），受审 SHA-256 `2c84d2c49b5c…`；据此修订为 R2 受审字节 | ① 「验证执行协议」第 4 项新增 G-03 计数口径：每条 AC 一个谓词，退出状态为执行条件（不计数），issue/Git 要求的特定数值退出码单列；据此把全部活动卡的复合 AC 拆为单谓词并重算（`BR-01` 4→7、`BR-04` 4→6、`BR-05` 4→7、`BR-06` 5→6、`BR-08` 3→6、`BR-09` 6→7、`BR-11` 5→6、`BR-12` 4→6、`BR-15` 3→4、`BR-22` 5→8、`BR-24` 2→4、`BR-25` 3→7、`BR-32` 7→8，均 ≤ 8）；② 删除 `revision.md`/`zh-CN/revision.md`/网站 `revision.en.md` 写集（`docs/commands/revision.md` 是 `libra revision` 序号索引命令页，修订语法由 `rev-parse.md` 承载）；③ `BR-07` 拆为仓库 X/Y 消除 AC5→AC6 上游冲突（按 `t3204#16` 形态），描述收窄至 `--set-upstream-to`/`--unset-upstream`；`BR-17` 新增位置 `<pattern>` 收窄 AC，`--forked <remote>` 与 `@{-N}` 创建/`-f` 路径登记 `DEFER-BR-07`；④ `BR-25` 按 Git `remote.c:672-711` 区分 `pushRemote`/`pushDefault`/`branch.remote` 优先级及唯一远端/`origin` 回退（7 条），并明确 `BR-40` 守卫用例由各解除卡同卡删减、`BR-26` 同卡更新 `BR-25` 依赖守卫的预期；`BR-29` 大小写变体收为两种以守 8 条上限；`BR-40` 删除混入行为的「前置」；⑤ `BR-02` 多通配符改 `'feat/*' 'fix/*'`、创建判定要求 `rev-parse` 成功且为 40 位十六进制；`BR-03` 描述去掉未验收的 `-vv`、字节串改 `\x1b` 字面量并以 `libra clone` 生成同提交的远程追踪 ref；`BR-05` 断言两键删除；`BR-11` reflog 只比较 `(old_oid, new_oid, message)`；`BR-19` 改用 `-refname` 次键夹具（可区分只取末键的实现）；`BR-22` 新增全名拒绝 AC；`BR-32` 新增显式 `main@{u}` 分支名 AC；⑥ 修正脚本误写的 `BR-24` 审计/追溯行与存根「38 张」 | N/A（无拆卡） | 执行检查必备需求、`BR-01`..`BR-32`、`BR-34`、`BR-36`、`BR-40`、审计表、追溯表、延后项、评审结论 |
| 2026-11-18 18:12:05 UTC | 双审 R2：Claude FAIL（2×P1）、Codex FAIL（A–F 六分区共 17 条 P1、1 条 P2），受审 SHA-256 `e7352ef5032e…`；据此修订为 R3 受审字节 | ① `BR-03` 落点按同目录计为 1（`landing=1; prod-files=2`）；② `BR-02` 创建判定改用与 HEAD 不同提交的 start-point `other`；③ `BR-07` AC1 改为 `rev-parse @{-1}` 的提交 ID（`--abbrev-ref` 走 `rev_parse.rs` 独立路径、不在写集），AC6 改为直接查询 `config_kv` 行数；④ `BR-09`/`BR-11` 配置迁移/复制改为整段（子键, 值）集合相等并加入自定义键夹具，reflog 改为比较除 `id`、`ref_name` 外全部七字段；`BR-12` 同名复制改为全字段前缀 + 恰追加一行；`BR-14` AC1 断言有序输出（P2 一并处理）；⑤ `BR-21` AC6/AC7 改为第二字段序列等式；`BR-18` 新增 `#699` 的 `branch.sort=-refname` + `--no-sort --sort=objecttype` AC；`BR-23` 新增 `t3203#33` 形态（detached 伪条目恒在首行、同值按 refname 升序）两条 AC；⑥ `BR-40` 八条过渡拒绝改为互不相同的整句 `@{push} is not yet supported for <配置>`，`BR-25` 同步引用整句；`BR-29` AC1 改为提交 ID 判定（`--symbolic-full-name` 走 `rev_parse.rs` 独立路径）并补回 `@{PuSH}`；`BR-27` AC3、`BR-26` AC6 同样改为提交 ID 判定；⑦ `DEP-BR-08` 的 `docs/error-codes.md` 写者补 `BR-08`；`DEP-BR-10` 扩至 `fetch_test.rs` 与 fetch 文档并纳入 `global-init-infra`、按 `DEP-GII-03` 口径；⑧ `DEFER-BR-02` 明确承接各 issue 的「上游单个测试 / TC 通过」验收项，相关 issue 在本计划完成后为部分闭环、保持 open；成功定义与完成判据同步 | N/A | 依赖表、`BR-02`/`03`/`07`/`09`/`11`/`12`/`14`/`18`/`21`/`23`/`25`/`26`/`27`/`29`/`40`、审计表、追溯表、延后项、成功定义、完成判据 |
| 2026-11-18 18:16:22 UTC | 双审 R3：Claude FAIL（1×P1、1×P2）、Codex A/C 分区 PASS、B/D/E/F 分区 FAIL（共 9 条 P1、2 条 P2），受审 SHA-256 `e75fc568e4ec…`；据此修订为 R4 受审字节 | ① `BR-01` 互斥条件改为「解析后列模式实际启用」，新增非终端 `--column=auto -v` 退出 0 的 AC（pinned Git 实测）；② `BR-03` 描述限定填充只适用于 `-v` 列表；③ `BR-09`/`BR-11` reflog 比较补 `worktree_id`（八字段），成功路径补 `reference` 表集合等式（`BR-09` 另补 HEAD 随重命名移动，配置迁移与旧名清理合并为一条集合等式），故障注入执行条件要求 stderr 含 `br-fi`；④ `BR-34` 同名源路径重命名补新 ref 指向原提交与旧 ref 移走两条 AC；⑤ `BR-18` 夹具改用 `libra config branch.sort -- -refname`（实测无 `--` 时 exit 129）；⑥ `BR-21` 「detached from」改为提交 ID 形态（tag 名形态需 `switch`/`checkout` 保留输入名，移入 `DEFER-BR-07`）；`BR-23` Verification 覆盖 AC1–AC6；⑦ `BR-26` mirror 夹具去掉 upstream，使忽略 mirror 的实现失败；`BR-28` 同卡更新既有 `test_for_each_ref_push_atom` 的三角预期并登记为共享回归组；`BR-40` 映射写明整句模板与 `<K>` 取值；评审结论与 waiver 行「7 项」改为「8 项（7 项未实现配置 + `push.default=upstream`）」 | N/A | `BR-01`/`03`/`09`/`11`/`18`/`21`/`23`/`26`/`28`/`34`/`40`、评审结论、waiver、延后项、审计表、追溯表 |
| 2026-11-18 18:19:30 UTC | 双审 R4：Claude PASS（2×P2）、Codex A/C 分区 PASS、B/D/E/F 分区 FAIL（共 5 条 P1），受审 SHA-256 `5f156fc2c226…`；据此修订为 R5 受审字节 | ① `BR-01` 描述注明终端下 `--column=auto -v` 的拒绝需 PTY 夹具、不作验收，登记 `DEFER-BR-08`；② `BR-09`/`BR-11` 配置迁移/复制由集合改为按 `id` 排序的（键, `value`, `encrypted`）序列等式，夹具加入同键多值（含重复）且全部为非加密行，加密行登记 `DEFER-BR-09`；③ `BR-22` 故障注入拆为 `reference` 与 `reflog` 两个注入点（覆盖两种删除顺序）并要求 stderr 含 `br-fi`，去掉非 issue 要求的数值退出码 AC 以守 8 条；④ `BR-28` 明确为既有 `test_for_each_ref_push_atom` 夹具补 `remote.<r>.fetch` 并把每条预期改为 pinned Git 同形实测输出 | N/A | `BR-01`/`09`/`11`/`22`/`28`、延后项、成功定义、完成判据、审计表、追溯表 |
| 2026-11-18 18:22:11 UTC | 双审 R5：Claude FAIL（1×P1）、Codex A/C 分区 PASS、B/D/E/F 分区 FAIL（共 4 条 P1），受审 SHA-256 `5e728616a48b…`；据此修订为 R6 受审字节 | ① `BR-02` 新增 `#679` Acceptance 2、3 的 `--list -v 'branch*'` 末字段 `[two, one]` AC（`t3203` 形态夹具）；② `BR-09`/`BR-11` 描述与 AC 限定为非加密配置行，Out of scope 引用 `DEFER-BR-09`；③ `BR-22` 远程追踪 ref 行按实际存储形式（`name` 为完整 `refs/remotes/origin/<b>`，`src/command/fetch.rs:4438-4446`）判定，修正 AC3 与 AC7 触发器条件；全名拒绝 AC 改在 ref 仍存在的新夹具上执行；④ `BR-28` 既有用例修订改为：三角场景为空串（可相同），优先级与大小写场景改在 `push.default=current` 下以互不相同的远程追踪 ref 区分 | N/A | `BR-02`/`09`/`11`/`22`/`28`、审计表、追溯表 |
| 2026-11-18 18:25:12 UTC | 双审 R6：Claude PASS、Codex B/C/E 分区 PASS、A/D/F 分区 FAIL（共 4 条 P1、3 条 P2），受审 SHA-256 `8e508ff78f5a…`；据此修订为 R7 受审字节 | ① `BR-11` 审计表与粒度轴改为「全量非加密配置」；② `BR-09`/`BR-11` 夹具加前置「执行前不存在目标名配置」，目标名已有孤立配置的合并语义（pinned Git 实测保留孤立段）登记 `DEFER-BR-10`；③ `BR-13` AC1/AC2 改为与同形夹具上 `-C` 的 SNAP 等价（去 `id` 与时间戳列），覆盖配置与 metadata 复制；④ 「验证执行协议」分类：卡特有用例只含 `(new)` 用例，为对齐本卡新语义而更新预期的既有用例归共享回归组（不计 VER，映射逐项写明改写） | N/A | 执行检查必备需求、`BR-09`/`11`/`13`、审计表、延后项 |
| 2026-11-18 18:27:10 UTC | 双审 R7：Codex A–F 六分区全部 PASS（无 P0/P1；3 条 P2 登记残余风险）、Claude FAIL（1×P1），受审 SHA-256 `cad7debafe55…`；据此修订为 R8 受审字节 | `BR-13` AC1/AC2 的 `-C` 对照仓库改为被测仓库在执行命令前的整目录字节副本（分别建库会因提交时间不同导致提交 ID 不同，使正确实现的 SNAP 等价断言失败）；其余字节不变 | N/A | `BR-13` |
| 2026-11-18 18:28:48 UTC | 双审 R8 PASS 记账 | Codex R8（差异确认，基于 R7 六分区全文 PASS）与 Claude R8（差异确认）对受审 SHA-256 `ea135d1e7ecdf0db6e6af27159786b1b828e8b14d41969f06650df92c3651832` 均 `VERDICT: PASS`；本行与头部状态行为 PASS 后仅有的记账修改 | N/A | 头部、修订历史、Review log |

## 已决议设计决策

### ADR-BR-01: 共享修订语法与格式原子下沉至公共引擎

- **Status:** Accepted
- **Context:** `#684`/`#685`（`@{upstream}`/`@{push}`/`@{-N}`）与 `#700`/`#701`（`%(ahead-behind:...)` / detached HEAD）属于全仓通用修订解析（`src/utils/util.rs`）与通用 ref 格式化引擎（`src/command/for_each_ref.rs`）。
- **Decision:** `BR-06`/`BR-07` 在 `src/utils/util.rs` 提供提交解析与分支名解释 helper 并接入 `src/command/branch.rs`；`BR-20`/`BR-21` 在 `src/command/for_each_ref.rs` 实现格式原子与 detached HEAD 渲染。
- **Alternatives considered:** 仅在 `branch.rs` 局部字符串替换。拒绝理由：违反 GC-02 单一事实源。
- **Consequences:** 触及共享 helper 命中 ER-13 `T-1`，对应卡执行 C 组时必须跑双环境完整 nextest。
- **Revisit when:** 若后续引入独立 revision parser crate 时再迁移。

### ADR-BR-02: `branch.rs` 单文件写集串行化推进

- **Status:** Accepted（2026-11-18 修订：撤回颜色家族 `REL-BR-03`）
- **Context:** 任务卡共享 `src/command/branch.rs` 与 `tests/command/branch_test.rs`。
- **Decision:** 按 G-10 禁止 I–I 并发，活动卡串行执行；每张活动卡独立 `patch + 1` 并触发 `release.yml`（G-07 默认），本计划不设任何 `REL-*` 合并发布。原 `REL-BR-03`（`BR-03`/`BR-37`/`BR-38` 子卡 + 发布点 `BR-39`）已撤回：`#680` 的 issue 验收项（`color.branch.current`/`local`/`remote` 与 `-v -a` 着色布局）由独立卡 `BR-03` 一次交付，超出 issue 的颜色轴登记 `DEFER-BR-03`。`BR-33`、`BR-35` 亦已撤回。
- **Alternatives considered:** 保留颜色家族共同首发。拒绝理由：家族覆盖的模式级联、`upstream`/`plain` 槽与 unborn/detached/column 着色路径超出 `#680` 明确诉求，且其中 unborn 路径与 Git（未出生分支不列出）语义相冲突，按范围控制改为 `DEFER-BR-03`。
- **Consequences:** 无并发冲突；全部活动卡按 `immutable-release` 前滚。
- **Revisit when:** 仅当未来拆分 `branch.rs` 为子模块后可调整。

## 全局工程约束

继承 `plan-template.md` 全部 `GC-01`..`GC-13`。补充本计划专项约束：
- **GC-BR-01:** 凡修改用户可见命令行为的卡，必须在同一卡更新对应命令 `docs/commands/<cmd>.md`（逐文件同步 EN 与 zh-CN）、`docs/development/commands/<cmd>.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`（按 ER-06a 核对相邻仓库 `/Volumes/Data/gitmono/libra-backend` 的 VCS 与 `cf` 分支，无有效 `cf` 仓库则 STOP 报告）及 `COMPATIBILITY.md`。
- **GC-BR-02:** 新增或修正错误码/退出码时，同步核对 `docs/error-codes.md`；不引入未解释的 `unwrap()`/`expect()`/`panic!()`（GC-11）。

## 执行检查必备需求（强制）

继承 `plan-template.md` 全部 `ER-01`..`ER-14`。
- 每张卡开工前执行 `ER-01`、`ER-02`（刷新行号锚点，并以 pinned Git `/Volumes/Data/tmp/libra-testcases/git-build-6de20f6092/git`（`git version 2.56.0.116.g6de20f6092`）复核本卡 AC 引用的 Git 行为）与 `ER-03`（复核 `G-*`）。
- 每张活动卡完成后按 `ER-04`、`ER-05`、`ER-07`、`ER-08`（`patch + 1`、`compat_version_surface_sync` 报告的版本面同步（当前 `Cargo.toml`、`install.sh`、`install.ps1`）、工具链刷新 `Cargo.lock`、`gh release create`）与 `ER-13`/`ER-14` 完成 C/D 组。已撤回的 `BR-33`、`BR-35`、`BR-37`、`BR-38`、`BR-39` 不开工、不发布。
- **验证执行协议（对全计划每条 `Verification` 条目强制）：**
  1. **分类：** 每条 `Verification` 标明两类之一——「卡特有用例」（`(new)` 用例，承载本卡 AC；只用精确过滤 `-E 'test(=<完整测试名>)'`）与「共享回归组」（锚定模块的正则过滤 `-E 'test(/^<模块路径>::/)'`，或既有具名用例的精确过滤——包括本卡按映射所写、为对齐本卡新语义而更新预期或删减分支的既有用例；它们不承载本卡 AC，只证明既有覆盖在新语义下仍成立，不计入 VER，改写内容须在映射中逐项写明）。测试名形式：集成测试为 `command::<module>::<fn>`（`tests/command_test.rs:10` 的 `mod command` 与 `tests/command/mod.rs:1011,1047,1049,1126`）；单元测试为 crate 内完整模块路径：`command::branch::tests::`（`src/command/branch.rs:3559-3560`）、`internal::branch::tests::`（`src/internal/branch.rs:980-981`）、`internal::reflog::tests::`（`src/internal/reflog.rs:903-904`）、`command::for_each_ref::color_spec_tests::`（`src/command/for_each_ref.rs:2392-2393`）、`command::fetch::tests::`（`src/command/fetch.rs:5516-5517`）、`utils::util::test::`（`src/utils/util.rs:4214-4215`）。
  2. **列举：** 运行任一条目前，先以相同 target 与过滤执行 `cargo nextest list`（如 `cargo nextest list --test command_test -E '<同一过滤>' --message-format oneline`）并记录列出的测试名：卡特有用例的列出集合必须与条目写明的完整测试名逐字相等（单名即恰好 1 条）；共享回归组必须 ≥ 1 条并记录条数。不满足即 STOP。
  3. **运行与判据：** 记录退出码与 nextest 汇总行 `N tests run: P passed, F failed, S skipped`（`N` 为 1 时写作 `1 test run`；某项为 0 时 nextest 可能省略该段，按 0 记）：要求退出码 = 0、`F` = 0、`N` = `P` = 第 2 步列出条数。`S` 是被过滤排除（及未选中的 `#[ignore]`）的测试数，只记录、不作判据。出现 `FLAKY` 须写明，`--retries` 不得掩盖最终失败。卡特有用例必须是 L1 确定性用例（不依赖 L2/L3 环境门，不得以打印 `skipped (...)` 提前返回）。
  4. **AC 绑定：** 每条卡特有用例在所在卡的「AC → 断言映射」中写明覆盖的 AC 编号、操作与判据；映射只写判定对应 AC 所需的检查，不列 AC 之外的独立断言（凡可独立失败的行为断言都必须是一条计数的 AC）；标注「前置」的检查只验证夹具成立，不是行为判据。退出码按数值判定是确定的：子进程用例经 `run_libra_command`/`run_libra_command_with_env`（`tests/command/mod.rs:354-399`）启动，`env_clear()` 后只注入固定变量，不继承 `LIBRA_FINE_EXIT_CODES`，按 `src/utils/error.rs:30-36` 默认使用 Git 约定（致命错误 128、用法错误 129）。 **计数口径（G-03）：** 每条 AC 只含一个可独立失败的谓词；被测命令的退出状态（成功类为 0、失败类为非 0）是判定该谓词的「执行条件」，不单独计数，AC 未逐条写出时同样适用；当 issue 验收项或 Git 对齐要求特定数值退出码（如 128 而非 129、1 而非 128）时，该数值单列为一条计数 AC；同一谓词对多个输入分别判定时按输入数计。
  5. **共享回归组** 属 ER-04 A 组 focused 门，只证明既有行为未回归，不计为卡特有 AC 证据，也不计入 G-03 的 `VER` 分子；判据同第 3 步。
  6. **夹具与快照约定（各卡映射引用）：** ① **F0**：新增集成用例以 `tempfile::tempdir()` 建仓，`run_libra_command(&argv, <repo>)` 子进程执行全部 `libra` 命令（显式 cwd），提交与分支用 `tests/command/mod.rs` 既有 helper 建立；不加 `#[serial]`，故本计划不修改 `tests/SERIAL_REGISTRY.tsv` 与 `.config/nextest.toml`（若实现发现必须加 `#[serial(...)]`，先按 ER-03 修订计划把这两个文件及 `sh tests/NEXTEST_GROUPS.sh` 纳入写集与验收）。② **FR**（远程追踪 ref）：另建本地路径仓库并提交，被测仓库 `libra remote add origin <path>` 后 `libra fetch origin` 生成 `refs/remotes/origin/*`（`libra update-ref` 只支持 `refs/heads`，`DEP-BR-05`）。③ **SNAP**：经 sea_orm 原生语句（写法同 `tests/command/agent_erasure_test.rs:13-22`）读取被测仓库 `.libra/libra.db` 的 `reference`、`reflog`、`config_kv`、`config`、`metadata_kv` 五张表的全部行（全列、按 `id` 排序）；「SNAP 相等」= 五表行逐字段相等；「SNAP 仅差 X」= 删除 X 所列行后逐字段相等。测试 harness 在仓库内建的 `.libra-test-home/` 不在上述表中，不影响 SNAP。④ **FI**（故障注入）：测试经同一 sea_orm 连接在 `.libra/libra.db` 安装 `CREATE TRIGGER br_fi BEFORE <INSERT|DELETE> ON reflog WHEN <NEW|OLD>.ref_name = '<完整 ref 名>' BEGIN SELECT RAISE(ABORT, 'br-fi'); END;`，在指定步骤制造确定性失败（`reflog.ref_name` 以完整 ref 名存储，`src/internal/reflog.rs:250,292`），不新增生产 failpoint 或环境变量；SNAP 取于安装触发器之后、执行被测命令之前。⑤ **NOESC**：stdout 字节不含 `0x1B`。
  7. **记录：** 命令、列出条数、汇总行、退出码与 UTC 时间写入本计划对应卡的 `Verification` 条目（v2.12，不新建证据文件）。
- **文档与网站验收命令（ER-06/ER-06a 同卡门；每张活动卡执行第 1–7 步）：** 以下 `<cmd>` 集合 = 本卡 `Implementation write set` 中网站页 `.../commands/<cmd>.en.md` 对应的命令名（`branch`、`rev-parse`、`for-each-ref`、`fetch` 中的若干个）；不得跨命令连带同步无关页面。本节所有按退出码判定的命令一律以 `if <命令>; then rc=0; else rc=$?; fi` 捕获退出码（模板「Verification 判定口径」），随后按各步取值判定。
  1. **仓库文档逐文件验收：** 对卡内 `Docs and compatibility impact` 列出的每个仓库内文件，记录修改前后 `shasum -a 256 <file>`；以 `rg -n -F '<本卡 flag 或语法关键词>' <file>`（锚点定位用，非判据）定位并人工确认对应章节已描述本卡行为（EN 与 zh-CN 均需覆盖；开发文档与 `COMPATIBILITY.md` 行可记录「无需修改」及理由）；`COMPATIBILITY.md` 只原地改写本卡命令的既有行、不增删行（提交前执行）：先 `if base="$(libra show HEAD:COMPATIBILITY.md)"; then rc=0; else rc=$?; fi`，`rc` 非 0 或 `base` 为空即 STOP；再要求 `printf '%s\n' "$base" | wc -l` 与 `wc -l < COMPATIBILITY.md` 相等，且对每个改动行号集合 `<L>` 执行 `if diff <(printf '%s\n' "$base" | sed '<L>d') <(sed '<L>d' COMPATIBILITY.md); then rc=0; else rc=$?; fi` 的 `rc` 为 0（基线只存于 shell 变量，不写文件；1 = 其它行被改即 STOP，>1 判执行失败）。缺失任一文件或内容不符即 STOP。
  2. **网站前置（第 3 步写入前完成）：** ① `cd /Volumes/Data/gitmono/libra-backend && test -d .libra && test ! -e .git`（退出 0 即仅含 Libra 元数据）；② `libra status --short --branch` 首行以 `## cf...origin/cf` 开头（不在 `cf` 时 STOP 并报告，不代为切换）；以 `libra status --porcelain -z --untracked-files=all` 记录全部既有无关改动（每条记录状态列、全部路径与指纹：普通文件为「类型 + 权限位 + `shasum -a 256`」，符号链接为「类型 + `readlink` 目标」，已不存在的路径记 `absent`，其它类型或无法读取即 STOP），只存于 shell 变量与本卡记录并保持不动；`libra remote -v` 的 `origin` 指向 `libra-tools/libra-backend`；③ 对每个 `<cmd>`，`libra status --short --no-branch -- apps/tanstack-app/content/docs/commands/<cmd>.en.md` 输出为空；④ 暂存区为空：`libra status --porcelain` 输出中不存在首列为非空格且非 `?` 的行；⑤ 远端对账：`libra fetch origin` 退出 0 后比较 `libra rev-parse HEAD` 与 `libra rev-parse origin/cf`——相等即通过并记录该 SHA；不等且 `libra merge-base --is-ancestor HEAD origin/cf` 退出 0（本地仅落后）时执行 `libra pull --ff-only origin cf`（须退出 0）后从 ② 重新核验；不等且该命令退出 1 即 STOP 并报告归属。任一条不满足或任一命令出现非预期退出码即 STOP。
  3. **网站同步与路径判定：** 先 `python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --backend-root /Volumes/Data/gitmono/libra-backend --check --only <cmd...>`（按退出码捕获）。**变更分支**（退出 1 = 有漂移）：写入前先按第 7 步为每个漂移页选定本卡新增句 `M_<cmd>` 并确认零命中，再以同一命令的 `--write --only <cmd...>` 写入，之后再次 `--check --only <cmd...>` 须退出 0；记录各网站文件修改前后 SHA-256，且每个漂移页的 `libra status --short --no-branch -- <页>` 须非空，随后执行第 4–7 步。**无变更分支**（退出 0）：不写入；人工确认网站页已描述本卡行为（未描述即 STOP 并核查），为每页摘取一句描述本卡行为的纯文本句 `W_<cmd>`（不含 Markdown 标记，也不含 `'`、`"`、`&`、`<`、`>`），`rg -F -q -- '<W>' <网站文件>` 须退出 0，再执行第 4 步并对线上页面执行第 7 步「页面检查」（判定句为 `W`）：含即记录为「网站未变更」证据；不含则按 ER-10 有界轮询（≤ 5 次、总计 ≤ 30 分钟），超预算即本卡 `blocked`；无变更分支不执行第 5、6 步。其它退出码即 STOP。漂移中若含本卡之外的既有仓库文档差异，一并同步并在第 5 步评审中说明（网站以仓库文档为唯一来源）。
  4. **前端验证：** 在 `/Volumes/Data/gitmono/libra-backend` 运行 `pnpm --filter @tinyship/tanstack-app typecheck`（`apps/tanstack-app/package.json:13`：`pnpm gen:docs && tsc --noEmit`）与 `pnpm --filter @tinyship/tanstack-app build`（`package.json:8`：`CF_DEPLOY=1 vite build`），均退出 0（无变更分支亦执行）。二者重写被忽略的派生产物 `apps/tanstack-app/src/generated/docs-manifest.ts`、`docs-search.ts`（`.gitignore:87-88`）与 `apps/tanstack-app/dist/`（`.libraignore:6`）；这三处已列入每张卡的 `Implementation write set`，只在持有 `DEP-BR-14` 窗口时执行，不得手工编辑、暂存或提交。
  5. **独立评审：** 对本卡网站改动（`libra diff -- <网站文件...>`）取得独立 Claude Code 字面 `VERDICT: PASS`，记录受审文件 SHA-256；评审后受审文件 SHA-256 变化即须重审。该网站交付门独立于计划文本的 Claude + Codex 双审，二者互不替代。
  6. **提交与推送：** `libra add <本卡网站文件精确路径...>` 后，`libra status --porcelain` 中首列非空格且非 `?` 的路径集合须恰为这些文件（否则 STOP）；`libra commit -s -m "docs(<cmd>): ..."` 后，`libra show --name-only --format=format: HEAD` 列出的非空行集合须恰为这些文件（否则 STOP，不得推送）；按 ER-07 核验同一提交的 `gpgsig` 与 `Signed-off-by`；以 `libra show HEAD:<path> | shasum -a 256` 确认每个文件的提交内容等于第 5 步受审 SHA-256；`libra push origin cf` 后 `libra ls-remote origin refs/heads/cf` 须恰输出一行且其 hash 等于 `libra rev-parse HEAD`；推送被拒即 STOP，不得强推。推送后以第 2 步同一命令与指纹规则重新枚举：除本卡网站文件外的条目集合须与第 2 步记录逐项相等，否则 STOP。
  7. **部署核验（仅变更分支；「页面检查」两分支通用）：** 「页面检查」页面内容只保存在 shell 变量中，不写任何文件（v2.12）：先 `if page="$(curl -fsSL 'https://libra.tools/en/docs/commands/<cmd>')"; then rc=0; else rc=$?; fi`，`rc` 非 0 即 STOP（不得视为「不含」）；再 `if rg -F -q -- '<判定句>' <<<"$page"; then hit=0; else hit=$?; fi`，`hit` 为 0 = 含、1 = 不含、>1 判执行失败；每次检查的 `rc`、`hit` 与 UTC 时间写入本卡记录。变更分支在第 3 步写入前为每个漂移页选定本卡新增的一句描述本卡行为的唯一纯文本句 `M_<cmd>`（取自第 1 步本卡新增的仓库 EN 文档文本，约束同 `W`），并确认 `rg -F -q -- '<M>' <网站文件>` 退出 1 且「页面检查」为不含；推送后按 ER-10 有界轮询（≤ 5 次、总计 ≤ 30 分钟）执行「页面检查」直到每页含其 `M`，记录命中时间与第 6 步的 `cf` 提交 SHA。若 `M` 在推送前已出现或超预算仍未出现，本卡置 `blocked`，不得标 `complete`。
  - **STOP 条件汇总：** 逐文件哈希不符、网站 `--check` 非预期退出、`typecheck`/`build` 失败、Claude Code 未返回 `VERDICT: PASS`、提交文件集合或提交内容与受审字节不一致、`cf` 推送失败或页面检查超预算时，立即 STOP，不得标 `complete`，并在 review log 记录失败环节与证据。
  - **计划文本提交门：** Claude（Subagent）与 Codex 对同一受审 SHA-256 均给出 `VERDICT: PASS`（用户 2026-11-18 指令）；该门与第 5 步的网站独立 Claude Code `PASS`、执行期 ER-05 代码评审互相不可替代。
- 全部 35 张活动卡完成后执行计划收口全量门：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`；三者绑定最终代码树（`docs/development/plan/` 以外路径的树内容），每次运行记录 `libra rev-parse HEAD` 与该树内容；之后仅 `docs/development/plan/` 下的记账提交不触发重跑，其它路径一旦变化即须重跑。

## 实施顺序

按 G-10 串行化写集，形成与 Phase 0/1/2 一致的无环顺序链：
- `BR-01 -> BR-02 -> BR-03 -> BR-04 -> BR-05 -> BR-06 -> BR-32 -> BR-31 -> BR-07 -> BR-08 -> BR-09 -> BR-10 -> BR-34 -> BR-36 -> BR-11 -> BR-12 -> BR-13 -> BR-14 -> BR-15 -> BR-16 -> BR-17 -> BR-18 -> BR-19 -> BR-20 -> BR-21 -> BR-22 -> BR-23 -> BR-24 -> BR-40 -> BR-25 -> BR-29 -> BR-27 -> BR-30 -> BR-26 -> BR-28`（已撤回的 `BR-33`、`BR-35`、`BR-37`、`BR-38`、`BR-39` 不在链上）

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-BR-01 | incoming | 上游基线 | `main@62abb622f0eb2626934633ada78061963ebf1f44` | 仓库维护者 | 干净可编译基线且 `compat_version_surface_sync` 通过 | `Cargo.toml:3` | 若上游版本面不一致，先建修复卡对齐再开工 |
| DEP-BR-02 | incoming | 相邻文档仓库 | `/Volumes/Data/gitmono/libra-backend`（唯一网站交付仓库，`cf` 分支；不接受其它检出替代）。依据：用户 2026-11-18 11:55:09 UTC 确认该路径为本批网站交付检出；`AGENTS.md:135` 的 `/Volumes/Sea/libra-backend`（本机不存在）为已知文档滞后，本计划不改 `AGENTS.md` | 文档发布者 | 已满足（路径已由用户确认）；每次写入前按「文档与网站验收命令」第 2 步核验路径存在、仅含 `.libra`、当前分支为 `cf` | 「文档与网站验收命令」第 2 步 | 核验不通过即 STOP 并报告，禁止改写其它检出 |
| DEP-BR-03 | incoming | 外部前置 Issue | `libra#686`（全局 `-C` 标志） | CLI 维护者 | `libra -C <dir> branch ...` 正常执行 | `gh issue view 696` | 仅影响全套 upstream `t3200#179` 运行；不影响 `BR-17` 本身单测/集成验收 |
| DEP-BR-04 | incoming | 外部前置 Issue | `libra#676`（`checkout --orphan` 级联前置） | CLI 维护者 | 上游完整 `t3204` 运行前置就绪 | Issue #680/#685 关联项 | 仅影响完整上游脚本全跑；本卡聚焦测试不阻塞 |
| DEP-BR-05 | incoming | 外部前置 Issue | `libra#541`（`update-ref` 仅限 `refs/heads`） | CLI 维护者 | 通用 ref 更新支持非 `refs/heads` 引用 | Issue #680/#689 关联项 | 仅影响上游脚本 setup；不影响 `BR-03` 本地测试 |
| DEP-BR-06 | incoming | 外部前置 Issue | `libra#675`（`branch -M` 强制重命名） | CLI 维护者 | `branch -M` 可用于上游测试 setup | Issue #688 关联项 | 仅影响完整上游脚本 setup；`-M` 相关判据已移入 `DEFER-BR-04`，`BR-09` 只交付 `-m` |
| DEP-BR-07 | incoming | 外部前置 Issue | `libra#556`（symref/detached-from 底层前置） | CLI 维护者 | 上游完整 `t3203` symref 用例前置就绪 | Issue #701 关联项 | 仅影响完整上游脚本跑通；`BR-21` 自身聚焦测试不阻塞 |
| DEP-BR-08 | incoming | 跨计划共享文档写集 | `COMPATIBILITY.md`、`docs/error-codes.md`（同批全部计划共享 `COMPATIBILITY.md`，按 global-init-infra `DEP-GII-03` 同口径；`docs/error-codes.md` 另与 `grep`/`checkout-switch-restore`/`diff` 等共享） | 仓库协调者 | 每张活动卡（`BR-01`..`BR-32`、`BR-34`、`BR-36`、`BR-40`；已撤回卡不写文件）开工编辑前取得协调者对上述文件的单写者串行窗口，本卡合入 `main` 后释放。`COMPATIBILITY.md` 仅原地改 `branch`/`for-each-ref`/`fetch`/`rev-parse` 专属行（不增删行），`docs/error-codes.md` 由 `BR-01` 追加 `LBR-CLI-002` 的 exit-128 override 说明，`BR-05`/`BR-08` 新增稳定码时追加行 | 协调者放行记录写入该卡 `Verification` 记录（合入后同步 `plan-status.md`） | 未获窗口前该卡保持 `pending`，不编辑相交文件（G-10）；超过 7 日未获窗口即报告协调者 |
| DEP-BR-09 | incoming | 跨计划共享源码写集 | `src/utils/util.rs`、`tests/command/rev_parse_test.rs`、rev-parse 文档（与 `clone`/`describe`/`merge-pull`/`global-init-infra`/`plumbing-misc` 共享；describe `DEP-DESC-UTIL` 已登记本计划 `BR-06`、`BR-07`、`BR-25`..`BR-32`、`BR-40`） | 仓库协调者 | `BR-06`/`BR-32`/`BR-31`/`BR-07`/`BR-40`/`BR-25`/`BR-29`/`BR-27`/`BR-30`/`BR-26`/`BR-28` 开工编辑前按 `DEP-GII-03` 口径核对同批写集交集并取得单写者串行窗口，合入 `main` 后释放 | 核对结果与放行记录写入该卡 `Verification` 记录 | 未获窗口前阻塞编辑上述文件（G-10）；超过 7 日即报告协调者 |
| DEP-BR-10 | incoming | 跨计划共享源码写集 | `src/command/fetch.rs`、`tests/command/fetch_test.rs` 与 fetch 文档（EN/zh-CN/开发/网站 `fetch.en.md`；与 `fetch-push`、`clone`、`global-init-infra` 共享，按 `DEP-GII-03` 口径） | 仓库协调者 | `BR-24` 开工编辑前核对同批写集交集并取得上述文件的单写者串行窗口，合入 `main` 后释放 | 核对结果与放行记录写入该卡 `Verification` 记录 | 未获窗口前阻塞编辑上述文件（G-10）；超过 7 日即报告协调者 |
| DEP-BR-11 | incoming | 跨计划共享源码写集 | `src/command/branch.rs`（与 `checkout-switch-restore#CSR-10` 共享） | 仓库协调者 | 修改 `branch.rs` 的卡片取得协调者串行放行并与 `CSR-10` 错开窗口 | 协调者串行调度记录 | 禁止与 `CSR-10` 并发修改 `src/command/branch.rs`（G-10） |
| DEP-BR-12 | incoming/outbound | 跨计划共享源码写集 | `src/internal/branch.rs`（与 `worktree-clean-reset-apply#WC-10`、`plumbing-misc#PM-21a` 共享；对方 `DEP-WC-01`/`DEP-PM-06` 所写拆分前编号 `PM-21` 即 `PM-21a`） | 仓库协调者 | `BR-10`/`BR-34`/`BR-36`/`BR-22` 修改 `internal/branch.rs` 前取得放行；顺序统一为 `BR-10`/`BR-22` → `WC-10` → `PM-21a`（与 `DEP-PM-06`、`DEP-WC-01` 一致，单向无环）；`BR-34`/`BR-36` 位于 `BR-10` 与 `BR-22` 之间，同样须在 `WC-10` 开工前合入或与之错开窗口 | 协调者串行调度记录 | 禁止并发修改 `src/internal/branch.rs`（G-10） |
| DEP-BR-13 | N/A | 已撤回 | 原为 `BR-03` 修改 `src/cli.rs`/`src/utils/output.rs` 的串行窗口；2026-11-18 `BR-03` 收窄为 `#680` 验收项后不再写这两个文件（颜色启用判定沿用既有 `OutputConfig`） | N/A | N/A | 修订历史 2026-11-18 行 | N/A |
| DEP-BR-14 | incoming | 跨计划网站交付串行窗口 | `/Volumes/Data/gitmono/libra-backend` 的 `cf` 推送与第 4 步派生产物（`apps/tanstack-app/src/generated/docs-manifest.ts`、`docs-search.ts`、`apps/tanstack-app/dist/`，按 global-init-infra `DEP-GII-03` 视为与同批全部计划的网站交付相交）及本卡网站页 | 仓库协调者 | 每张活动卡执行「文档与网站验收命令」第 2–7 步前取得协调者对网站交付的单写者窗口，第 7 步完成（或无变更分支的页面检查完成）后释放 | 放行记录写入该卡 `Verification` 记录 | 未获窗口前不执行第 2–7 步，本卡保持 `in-progress` 或 `blocked`；超过 7 日即报告协调者 |

### 发布分组与并发窗口

默认每张卡独立发布切片（G-07：`Release boundary = independent`，`Version increment = patch`）。本计划不登记任何 `REL-*`：原颜色家族 `REL-BR-03`（`BR-03`/`BR-37`/`BR-38` + 发布点 `BR-39`）已于 2026-11-18 撤回（见 `ADR-BR-02`）。失败时按 `immutable-release` 前滚 `patch + 1` 并给降级指引。

- **并发声明:** 全串行（按 G-10 禁止 I–I 并发）。
- **发布者:** 单一串行发布者。
- **发布窗口顺序:** `BR-01` → `BR-02` → `BR-03` → `BR-04` → `BR-05` → `BR-06` → `BR-32` → `BR-31` → `BR-07` → `BR-08` → `BR-09` → `BR-10` → `BR-34` → `BR-36` → `BR-11` → `BR-12` → `BR-13` → `BR-14` → `BR-15` → `BR-16` → `BR-17` → `BR-18` → `BR-19` → `BR-20` → `BR-21` → `BR-22` → `BR-23` → `BR-24` → `BR-40` → `BR-25` → `BR-29` → `BR-27` → `BR-30` → `BR-26` → `BR-28`，同一时刻仅允许 1 张卡处于已 bump 未完成推送/发布状态（ER-12）。

### Phase 0: 参数校验、查询与上游基础修复（BR-01 .. BR-05）

**目标:** 修复 `--column -v` 互斥、`--list <pattern>`、`#680` 的 `color.branch.current`/`local`/`remote` 着色、`--set-upstream-to refs/heads/` 与 `--unset-upstream` 无上游报错（`-v`/`-vv` 名称列对齐已由基线满足，`BR-33` 已撤回；`BR-37`/`BR-38`/`BR-39` 已撤回）。

**进入条件:**

- 本计划文本通过 Claude（Subagent）与 Codex 对同一字节的双审（均 `VERDICT: PASS`），且 `DEP-BR-01` 基线 parity 预检通过。

**退出条件:**

- `BR-01`..`BR-05` 全部达到 `Lifecycle=done`、`Acceptance=complete`。已撤回的 `BR-33`、`BR-37`、`BR-38`、`BR-39` 不是本阶段完成门。

### Phase 1: 修订语法与分支生命周期操作（BR-06 .. BR-15, BR-31, BR-32）

**目标:** 补齐 `@{upstream}`/`@{u}`/`@{-N}` 修订与分支名解释、`-m` 缺参/reflog/D/F 碰撞、`--copy` reflog/config/同名/`-c -f`、`-i` 与 `--show-current` 静默。

**进入条件:**

- Phase 0 退出条件满足。

**退出条件:**

- `BR-06`..`BR-15`、`BR-31`、`BR-32`、`BR-34`、`BR-36` 全部 `done/complete`，且各自独立发布。`BR-35` 已撤回，不是本阶段完成门。

### Phase 2: 过滤、排序、格式化、远程分支删除与 `@{push}`（BR-16 .. BR-30）

**目标:** 修复重复 `--merged`、`--forked`、`--sort=objecttype`、重复 `--sort`、`%(ahead-behind)`、`--sort=ahead-behind`、`--format` detached HEAD、`-r -d`、`skipFetchAll` 与 `@{push}`/`%(push)`，并完成计划全量收口。

**进入条件:**

- Phase 1 退出条件满足。

**退出条件:**

- `BR-01`..`BR-32`、`BR-34`、`BR-36`、`BR-40` 全部 `done/complete`，收口全量门全绿。已撤回的 `BR-33`、`BR-35`、`BR-37`、`BR-38`、`BR-39` 不进入本退出条件。

## 任务卡

### 字段全局默认与例外

- **Release boundary 默认:** `independent`
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（每卡独立发布 `v<version>` tag 与 Cloudflare artifact 不可撤回；失败一律 `patch + 1` 前滚修复，严禁删 tag。降级指引：受影响用户可通过 `install.sh` 指定上一已知良好 `v<prev>` 版本降级；兼容窗口：保持 CLI/SQLite/索引格式向后兼容，支持直接降级至上一 patch 版本而无需数据迁移）
- **Migration and rollback 默认:** `N/A：无 schema 迁移`
- **Security and privacy 默认:** `继承 GC-07、GC-11`
- **Performance budget 默认:** `继承 GC-10`

**默认覆盖：**

| 任务 | 偏离的字段 | 取值与理由 |
|---|---|---|
| `BR-03`, `BR-05`, `BR-06`, `BR-32`, `BR-31`, `BR-07`, `BR-08`, `BR-09`, `BR-10`, `BR-34`, `BR-36`, `BR-11`, `BR-20`, `BR-21`, `BR-22`, `BR-40`, `BR-25`, `BR-29`, `BR-27`, `BR-30`, `BR-26`, `BR-28` | `Full-suite trigger` | `T-1`：修改跨切面共享 helper（`util.rs` / `for_each_ref.rs` / `internal/branch.rs` / `internal/reflog.rs`）或 `docs/error-codes.md`；其余卡亦因 `patch + 1` 修改 `install.sh`/`install.ps1` 命中 `T-1` |

**D 组远端后置门默认（适用于全部 `independent` 活动卡，ER-04）：**
- `base.yml`（PR 与 `push` `main`）：`format`、`clippy`、`redundancy`、`test` 同一 head SHA 全绿（以现场 `.github/workflows/base.yml` 实际 job 为准）。
- `codeql.yml`（PR 与 `push` `main`）：CodeQL 安全扫描全绿。
- `release.yml`（`push` `v<version>` tag，由 `gh release create` 触发）：多平台编译、Cloudflare R2 上传与 Homebrew tap 更新全绿。

**规则 waiver（`EX-*`）：** 无（原 `EX-BR-01` 已取消：8 项过渡守卫（7 项未实现配置 + `push.default=upstream`）改由 `BR-40` 在 `BR-25` 前独立交付，无需超限豁免）。

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| BR-01 | implementation | `--column` 与 `-v` 互斥校验 | patch+1前滚修复 | yes | yes | 8/8 | 1/8 | 1/1 | M | DEP-BR-01,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 串行起点 | independent | N/A | N/A |
| BR-02 | implementation | `--list` 位置模式过滤 | patch+1前滚修复 | yes | yes | 8/8 | 1/8 | 1/1 | M | BR-01,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-01 | independent | N/A | N/A |
| BR-03 | implementation | color.branch.current/local/remote 槽着色（#680） | patch+1前滚修复 | yes | yes | 7/8 | 4/8 | 1/2 | M | BR-02,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-02 | independent | N/A | N/A |
| BR-33 | superseded | 基线已对齐，不得开工 | N/A | N/A | N/A | N/A | N/A | N/A | N/A | N/A | 不写文件 | none | BR-03 | N/A |
| BR-37 | withdrawn | 已撤回（颜色家族 `REL-BR-03` 撤回），无写集 | N/A | N/A | N/A | N/A | N/A | 0/0 | N/A | N/A | 无 | withdrawn | N/A | N/A |
| BR-38 | withdrawn | 已撤回（颜色家族 `REL-BR-03` 撤回），无写集 | N/A | N/A | N/A | N/A | N/A | 0/0 | N/A | N/A | 无 | withdrawn | N/A | N/A |
| BR-39 | withdrawn | 已撤回（颜色家族 `REL-BR-03` 撤回），无写集 | N/A | N/A | N/A | N/A | N/A | 0/0 | N/A | N/A | 无 | withdrawn | N/A | N/A |
| BR-04 | implementation | `--set-upstream-to refs/heads/` | patch+1前滚修复 | yes | yes | 6/8 | 1/8 | 1/1 | M | BR-03,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-03 | independent | N/A | N/A |
| BR-05 | implementation | `--unset-upstream` 无上游报错 | patch+1前滚修复 | yes | yes | 7/8 | 2/8 | 1/1 | M | BR-04,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-04 | independent | N/A | N/A |
| BR-06 | implementation | `@{upstream}` 修订与分支名解释 | patch+1前滚修复 | yes | yes | 6/8 | 2/8 | 2/2 | M | BR-05,DEP-BR-08,DEP-BR-09,DEP-BR-11,DEP-BR-14 | 序列化于 BR-05 | independent | N/A | N/A |
| BR-32 | implementation | `@{u}` 与大小写变体 `@{UPSTREAM}`/`@{U}` | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 2/2 | M | BR-06,DEP-BR-08,DEP-BR-09,DEP-BR-11,DEP-BR-14 | 序列化于 BR-06 | independent | BR-06 | N/A |
| BR-31 | implementation | `@{upstream}` detached HEAD 规则 | patch+1前滚修复 | yes | yes | 5/8 | 2/8 | 2/2 | M | BR-32,DEP-BR-08,DEP-BR-09,DEP-BR-11,DEP-BR-14 | 序列化于 BR-32 | independent | BR-06 | N/A |
| BR-07 | implementation | `@{-N}` 与 `@{-1}@{upstream}` 解释 | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 2/2 | M | BR-31,DEP-BR-08,DEP-BR-09,DEP-BR-11,DEP-BR-14 | 序列化于 BR-31 | independent | N/A | N/A |
| BR-08 | implementation | `-m` 缺参退出码 128 与提示 | patch+1前滚修复 | yes | yes | 6/8 | 2/8 | 1/1 | M | BR-07,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-07 | independent | N/A | N/A |
| BR-09 | implementation | `-m` 重命名迁移 reflog | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 2/2 | M | BR-08,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-08 | independent | N/A | N/A |
| BR-10 | implementation | 创建分支 D/F 冲突检查与错误点名 | patch+1前滚修复 | yes | yes | 4/8 | 1/8 | 2/2 | M | BR-09,DEP-BR-08,DEP-BR-11,DEP-BR-12,DEP-BR-14 | 序列化于 BR-09 | independent | N/A | N/A |
| BR-34 | implementation | 重命名 D/F 冲突检查、错误点名与源排除 | patch+1前滚修复 | yes | yes | 6/8 | 1/8 | 2/2 | M | BR-10,DEP-BR-08,DEP-BR-11,DEP-BR-12,DEP-BR-14 | 序列化于 BR-10 | independent | BR-10 | N/A |
| BR-35 | withdrawn | 已撤回，无写集 | N/A | N/A | N/A | N/A | N/A | 0/0 | N/A | N/A | 无 | withdrawn | N/A | N/A |
| BR-36 | implementation | 复制 D/F 冲突检查与错误点名 | patch+1前滚修复 | yes | yes | 8/8 | 1/8 | 2/2 | M | BR-34,DEP-BR-08,DEP-BR-11,DEP-BR-12,DEP-BR-14 | 序列化于 BR-34 | independent | BR-34 | N/A |
| BR-11 | implementation | `--copy` 复制 reflog 与全量非加密配置 | patch+1前滚修复 | yes | yes | 7/8 | 2/8 | 2/2 | M | BR-36,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-36 | independent | N/A | N/A |
| BR-12 | implementation | `-c` 同名复制 (`m2 m2`) 成功 | patch+1前滚修复 | yes | yes | 6/8 | 1/8 | 1/1 | M | BR-11,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-11 | independent | N/A | N/A |
| BR-13 | implementation | `-c -f` 强制复制等价 `-C` | patch+1前滚修复 | yes | yes | 4/8 | 1/8 | 1/1 | M | BR-12,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-12 | independent | N/A | N/A |
| BR-14 | implementation | `-i` 短选项别名绑定 `--ignore-case` | patch+1前滚修复 | yes | yes | 2/8 | 1/8 | 1/1 | M | BR-13,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-13 | independent | N/A | N/A |
| BR-15 | implementation | `--show-current` detached HEAD 静默 | patch+1前滚修复 | yes | yes | 4/8 | 1/8 | 1/1 | M | BR-14,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-14 | independent | N/A | N/A |
| BR-16 | implementation | 重复 `--merged` / `--no-merged` 过滤 | patch+1前滚修复 | yes | yes | 2/8 | 1/8 | 1/1 | M | BR-15,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-15 | independent | N/A | N/A |
| BR-17 | implementation | `--forked <pattern>` 分叉列表 | patch+1前滚修复 | yes | yes | 5/8 | 1/8 | 1/1 | M | BR-16,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-16 | independent | N/A | N/A |
| BR-18 | implementation | `--sort=objecttype` 排序键 | patch+1前滚修复 | yes | yes | 3/8 | 1/8 | 1/1 | M | BR-17,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-17 | independent | N/A | N/A |
| BR-19 | implementation | 重复 `--sort` 多键排序 | patch+1前滚修复 | yes | yes | 3/8 | 1/8 | 1/1 | M | BR-18,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-18 | independent | N/A | N/A |
| BR-20 | implementation | `%(ahead-behind:...)` 格式原子 | patch+1前滚修复 | yes | yes | 4/8 | 2/8 | 1/2 | M | BR-19,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-19 | independent | N/A | N/A |
| BR-21 | implementation | `--format` 包含 detached HEAD 伪条目 | patch+1前滚修复 | yes | yes | 8/8 | 1/8 | 1/2 | M | BR-20,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-20 | independent | N/A | N/A |
| BR-22 | implementation | `branch -r -d`/`-D` 删除远程追踪分支 | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 2/2 | M | BR-21,DEP-BR-08,DEP-BR-11,DEP-BR-12,DEP-BR-14 | 序列化于 BR-21 | independent | N/A | N/A |
| BR-23 | implementation | `--sort=[-]ahead-behind:<rev>` 排序键 | patch+1前滚修复 | yes | yes | 6/8 | 1/8 | 1/1 | M | BR-22,DEP-BR-08,DEP-BR-11,DEP-BR-14 | 序列化于 BR-22 | independent | BR-20 | N/A |
| BR-24 | implementation | `fetch --all` 尊重 `skipFetchAll` | patch+1前滚修复 | yes | yes | 4/8 | 1/8 | 1/1 | M | BR-23,DEP-BR-08,DEP-BR-10,DEP-BR-14 | 序列化于 BR-23 | independent | BR-22 | N/A |
| BR-40 | implementation | `@{push}` 过渡 fail-closed 守卫 | patch+1前滚修复 | yes | yes | 8/8 | 1/8 | 1/1 | M | BR-24,DEP-BR-08,DEP-BR-09,DEP-BR-14 | 序列化于 BR-24 | independent | BR-25 | N/A |
| BR-25 | implementation | `@{push}` remote 选择与 `upstream` | patch+1前滚修复 | yes | yes | 7/8 | 1/8 | 1/1 | M | BR-40,DEP-BR-08,DEP-BR-09,DEP-BR-14 | 序列化于 BR-40 | independent | BR-06 | N/A |
| BR-29 | implementation | `@{push}` 显式分支/大小写与分支名解释 | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 2/2 | M | BR-25,DEP-BR-08,DEP-BR-09,DEP-BR-11,DEP-BR-14 | 序列化于 BR-25 | independent | BR-25 | N/A |
| BR-27 | implementation | `@{push}` `simple` 与 `tracking_for_push_dest` | patch+1前滚修复 | yes | yes | 7/8 | 1/8 | 1/1 | M | BR-29,DEP-BR-08,DEP-BR-09,DEP-BR-14 | 序列化于 BR-29 | independent | BR-25 | N/A |
| BR-30 | implementation | `@{push}` detached HEAD 规则 | patch+1前滚修复 | yes | yes | 2/8 | 1/8 | 1/1 | M | BR-27,DEP-BR-08,DEP-BR-09,DEP-BR-14 | 序列化于 BR-27 | independent | BR-27 | N/A |
| BR-26 | implementation | `nothing`/`matching`/`current`/refspec/`mirror` | patch+1前滚修复 | yes | yes | 6/8 | 1/8 | 1/1 | M | BR-30,DEP-BR-08,DEP-BR-09,DEP-BR-14 | 序列化于 BR-30 | independent | BR-25 | N/A |
| BR-28 | implementation | `for-each-ref %(push)` 复用与错误透传 | patch+1前滚修复 | yes | yes | 3/8 | 1/8 | 2/2 | M | BR-26,DEP-BR-08,DEP-BR-09,DEP-BR-14 | 序列化于 BR-26 | independent | BR-26 | N/A |

---

### Task BR-01: 修复 `libra branch` 中 `--column` 与 `-v` 选项互斥校验（#678）

**Task type:** implementation
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`0.30.212`；targeted `command::branch_test::column_verbose_conflict` 1/1 PASS；Codex R4 `VERDICT: PASS`；`docs/error-codes.md`/EN/zh/开发/COMPATIBILITY/网站已同步；远端 CI 未等待，按用户 2026-11-20 指令 local C/D）
**Description:** 解决 Issue #678：当解析后的列模式实际启用（`--column`/`--column=always`，或 `--column=auto` 且 stdout 为终端）并同时传入 `-v`/`--verbose` 时，拒绝该选项冲突（`--no-column`、`--column=never` 与非终端下的 `--column=auto` 不触发；终端下 `--column=auto -v` 的拒绝需 PTY 夹具，本卡不作验收，见 `DEFER-BR-08`）并输出 `options '--column' and '--verbose' cannot be used together`（exit 128，对齐 Git `die`）。
**Out of scope:** `--format` 与 `--column` 的交互（已由既有逻辑覆盖）；`color.branch.*` 配置（由 `BR-03` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `BranchArgs.column` 与 `verbose` 未在 clap 或 `execute_safe` 中做互斥校验 | `src/command/branch.rs:491-508,875-886` |
| `render_branch_output` 仅以 `verbose == 0` 静默禁用列布局 | `src/command/branch.rs:2913-2916` |
**Acceptance criteria:**
- [x] `libra branch --column -v` 的退出码为 128（`#678` 与 pinned Git 一致）。
- [x] `libra branch --column -v` 的 stderr 含 `options '--column' and '--verbose' cannot be used together`（执行条件：以非零状态退出）。
- [x] `libra branch --column=always -v` 的 stderr 含同一诊断（执行条件：以非零状态退出）。
- [x] `libra branch --column --no-column -v` 退出 0（后给出的 `--no-column` 生效）。
- [x] `libra branch --column=never -v` 退出 0。
- [x] `libra branch --column` 单独使用退出 0（`#678` Acceptance 2）。
- [x] stdout 为管道（子进程默认）时 `libra branch --column=auto -v` 退出 0（`auto` 在非终端时不启用列布局，因此不触发互斥；pinned Git `builtin/branch.c:1117-1122` 先 `finalize_colopts` 再判 `explicitly_enable_column`，实测同样退出 0）。
- [x] `libra branch -v` 单独使用退出 0（`#678` Acceptance 2）。
- [x] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；`main` 上一个提交，另建分支 `topic`。
- AC1–AC8 → `command::branch_test::column_verbose_conflict`：逐条运行 AC 所列命令；AC1 判据 `status.code() == Some(128)`；AC2、AC3 判据 stderr 含该诊断原文（执行条件 exit ≠ 0）；AC4–AC8 判据 `status.code() == Some(0)`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::column_verbose_conflict)'`（卡特有用例，new；覆盖 AC1–AC8）
- [x] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** DEP-BR-01, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `docs/error-codes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`docs/error-codes.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`（按 ER-06a 核对相邻仓库 `/Volumes/Data/gitmono/libra-backend` 的 VCS 与 `cf` 分支，无有效 `cf` 仓库则 STOP 报告）及 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--column与-v互斥校验; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DEP-BR-01,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=串行起点; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-02: 支持 `libra branch --list` 接收位置模式参数（#679）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #679：修复 `new_branch`（`action` 组）与 `--list`（`query` 组）在 clap 层的冲突，使 `libra branch --list <pattern>...` 支持单个及多个通配符位置模式过滤分支列表。
**Out of scope:** `-i`/`--ignore-case` 短标志别名（由 `BR-14` 承接）；`--forked`（由 `BR-17` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `BranchArgs` 将 `new_branch` 放入 `group = "action"` 且 `ArgGroup("action").conflicts_with("query")`，导致 `--list <pattern>` 在 clap 解析阶段即被拒绝 | `src/command/branch.rs:320-365` |
**Acceptance criteria:**
- [ ] `libra branch --list 'feat/*'` 的 stdout 各行去掉行首两列标记（`* ` 或两个空格）后的名称集合恰为 `{feat/a, feat/b}`（执行条件：退出码 0）。
- [ ] `libra branch --list 'feat/*' 'fix/*'` 的名称集合恰为 `{feat/a, feat/b, fix/c}`（多个通配符取并集）（执行条件：退出码 0）。
- [ ] `libra branch -l 'feat/*'` 的 stdout 与 AC1 命令的 stdout 字节相同（执行条件：退出码 0）。
- [ ] `libra branch --list -r 'origin/*'` 的名称集合恰为 `{origin/main}`（执行条件：退出码 0）。
- [ ] `libra branch --list -a '*main'` 的名称集合恰为 `{main, remotes/origin/main}`（pinned Git 实测同集合）（执行条件：退出码 0）。
- [ ] `libra branch --list 'nomatch*'` 的 stdout 为空（执行条件：退出码 0）。
- [ ] 未带 `--list` 的 `libra branch newb other` 按创建语义执行：之后 `libra rev-parse newb` 与 `libra rev-parse other` 输出同一提交 ID（`other` 与当前 HEAD 指向不同提交，可区分忽略 start-point 的实现；执行条件：三条命令退出 0，且两条 `rev-parse` 的 stdout 为非空 40 位十六进制）。
- [ ] 仓库 T（`t3203` 形态：`main` 上提交 `one`、`two`，`branch-one` 在 `two`、`branch-two` 在 `one`）中 `libra branch --list -v 'branch*'` 各行最后一个空白分隔字段的序列恰为 `[two, one]`（只列出两个匹配分支且摘要正确；`#679` Acceptance 2、3）（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；`main`（当前）上提交 `c1`、`c2`（`main` 在 `c2`）；另建 `feat/a`、`feat/b`、`fix/c` 于 `c2`，`other` 于 `c1`；FR 生成 `refs/remotes/origin/main`（前置：`libra branch -r` 的名称集合恰为 `{origin/main}`）。
- 仓库 T：F0；`main` 上依次提交 `one`、`two`（说明即为提交信息）；`libra branch branch-one`（在 `two`）、`libra branch branch-two <one 的提交>`。
- AC1–AC8 → `command::branch_test::list_pattern`：逐条运行 AC 所列命令；AC1、AC2、AC4、AC5 判据名称集合相等；AC3 判据两次 stdout 字节相等；AC6 判据 stdout 长度为 0；AC7 判据两条 `rev-parse` 的 stdout 相等；AC8 在仓库 T 运行，判据末字段序列相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::list_pattern)'`（卡特有用例，new；覆盖 AC1–AC8）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-01, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--list位置模式过滤; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-01,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-01; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-03: 支持 `color.branch.current`/`local`/`remote` 着色（#680）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #680 的验收项：颜色启用时（`--color`/`--color=always`，或缺省 `auto` 且 stdout 为终端），`libra branch` 的普通列表与 `-v` 列表读取 `color.branch.current`、`color.branch.local`、`color.branch.remote`，并按 Git 的默认槽颜色与转义序列（起始 `ESC[<n>m`、复位 `ESC[m`，颜色名解析复用 `for_each_ref` 已有的 Git 颜色解析）渲染当前、本地、远程追踪分支名；`-v` 列表中名称按最长项填充且填充位于着色区间内，普通列表不填充（与 pinned Git `builtin/branch.c` `build_format` 一致）。颜色是否启用沿用既有全局 `OutputConfig`（`--color`/`--no-color`/`NO_COLOR`/`TERM=dumb`/`--machine`），本卡不改该判定。
**Out of scope:** `color.branch`/`color.ui` 模式级联、显式 `--color=auto` 与缺省区分、`NO_COLOR`/`TERM=dumb`/`--machine` 与 `color.branch` 的优先级、`color.branch.upstream`/`plain` 槽、unborn/detached HEAD/`--column` 着色路径（均为 `DEFER-BR-03`，原 `BR-37`/`BR-38`/`BR-39` 已撤回）；`--format` 中的 `%(color:...)`（已由 `for_each_ref` 处理）；基线 `list_name_column_width`（`branch.rs:3324-3339`）的名称列宽计算本身不改。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| 普通/`-v` 列表硬编码 `name.green()`（当前）与 `name.red()`（远程），本地非当前分支无复位序列，复位用 `ESC[0m`，未读取 `color.branch.*` | `src/command/branch.rs:2905-2967`；`#680` 探针 `* ^[[32mmain^[[0m` |
| Git 颜色名到 ANSI 的解析已存在但为私有函数 | `src/command/for_each_ref.rs:2143`（`color_spec_to_ansi`）、测试模块 `src/command/for_each_ref.rs:2392` |
| pinned Git 默认输出：`* ESC[32mmain ESC[m`、`  x ESC[m`、`  ESC[31mremotes/origin/main ESC[m`（本地非当前分支无起始序列、仍有复位）；`-v`/`-vv` 时填充位于复位之前（`  x   ESC[m 5b2aeba one`） | `/Volumes/Data/tmp/libra-testcases/git-build-6de20f6092/git branch --color=always -a` 与 `-vv` 实测（2026-11-18） |
**Acceptance criteria:**
- [ ] 未配置任何 `color.branch.*` 时，`libra branch --color=always -a` 的 stdout 字节恰为 `"* \x1b[32mmain\x1b[m\n  x\x1b[m\n  \x1b[31mremotes/origin/main\x1b[m\n"`（Rust 字节串字面量记法；与 pinned Git 对同形夹具的输出字节相同，本地非当前分支无起始序列但有复位 `\x1b[m`）（执行条件：退出码 0）。
- [ ] `color.branch.current=cyan` 时，`libra branch --color=always` 的当前分支行字节恰为 `"* \x1b[36mmain\x1b[m"`（执行条件：退出码 0）。
- [ ] `color.branch.local=blue` 时，`libra branch --color=always` 的本地非当前分支行字节恰为 `"  \x1b[34mx\x1b[m"`（执行条件：退出码 0）。
- [ ] `color.branch.remote=yellow` 时，`libra branch --color=always -r` 的 stdout 字节恰为 `"  \x1b[33morigin/main\x1b[m\n"`（执行条件：退出码 0）。
- [ ] 三槽同时配置（`cyan`/`blue`/`yellow`）时，`libra branch --color=always -v -a` 的 stdout 字节恰为把 `<s>` 替换为 `libra rev-parse --short HEAD` 输出后的 `"* \x1b[36mmain               \x1b[m <s> one\n  \x1b[34mx                  \x1b[m <s> one\n  \x1b[33mremotes/origin/main\x1b[m <s> one\n"`（名称按最长项 `remotes/origin/main` 填充且填充在复位之前；`#680` Acceptance 1、2、4；pinned Git 实测同形）（执行条件：退出码 0）。
- [ ] 三槽已配置时 `libra branch --color=never -v -a` 的 stdout 满足 NOESC（执行条件：退出码 0）。
- [ ] 三槽已配置、未传 `--color` 且 stdout 为管道（子进程默认）时，`libra branch -v -a` 的 stdout 满足 NOESC（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；`main`（当前）上一个提交 `one`，另建本地分支 `x`；远端仓库以 `libra clone <被测仓库> <远端>` 生成，被测仓库再 `libra remote add origin <远端>` + `libra fetch origin`，使 `refs/remotes/origin/main` 与本地 `main` 同为 `one`（前置：`libra branch -r` 的名称集合恰为 `{origin/main}`，`libra rev-parse origin/main` 等于 `libra rev-parse main`）。
- AC1 → `command::branch_test::color_branch_default_slots_match_git`：判据 stdout 字节等于 AC 所列字节串。
- AC2–AC4 → `command::branch_test::color_branch_custom_slots`：每条只配置对应一个槽；AC2、AC3 判据 stdout 按 `\n` 切分后恰有一行与所述字节逐字节相等；AC4 判据 stdout 字节相等。
- AC5 → `command::branch_test::color_branch_verbose_all_layout`：判据 stdout 字节等于替换 `<s>` 后的字节串。
- AC6、AC7 → `command::branch_test::color_branch_never_and_pipe_no_ansi`：判据 NOESC。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::color_branch_default_slots_match_git)'`（卡特有用例，new；覆盖 AC1）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::color_branch_custom_slots)'`（卡特有用例，new；覆盖 AC2–AC4）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::color_branch_verbose_all_layout)'`（卡特有用例，new；覆盖 AC5）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::color_branch_never_and_pipe_no_ansi)'`（卡特有用例，new；覆盖 AC6、AC7）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::for_each_ref::color_spec_tests::/)'`（共享回归组，不计入 G-03 VER）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享格式化引擎 `src/command/for_each_ref.rs`（仅把 `color_spec_to_ansi` 提升为 `pub(crate)` 供复用，不改其行为）；另因 `patch + 1` 修改 `install.sh`/`install.ps1`
**Dependencies:** BR-02, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/command/for_each_ref.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** `src/command/branch.rs`, `src/command/for_each_ref.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md` 的 `branch` 行（说明 `color.branch.current`/`local`/`remote` 与默认槽，以及 `DEFER-BR-03` 所列未支持项）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点 `src/command/`，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=color.branch.current/local/remote 槽着色（#680）; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=BR-02,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-02; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-37: 已撤回（不得开工）

**Status:** withdrawn。2026-11-18 双审预审按范围控制撤回原颜色家族子卡（`color.branch` 五个槽自定义样式）：`#680` 只要求 `current`/`local`/`remote` 三槽，已由独立卡 `BR-03` 承接；`upstream`/`plain` 槽及 never/auto 泄漏矩阵登记 `DEFER-BR-03`。本编号不再拥有写集、验收标准或发布边界，不计入 35 张活动卡。

---

### Task BR-38: 已撤回（不得开工）

**Status:** withdrawn。2026-11-18 双审预审撤回（原：普通/unborn/detached/column 四类列表默认与自定义颜色矩阵）：超出 `#680` 验收项，且 Git 对未出生分支不列出该分支（pinned Git 实测），与原 AC 前提冲突；普通列表默认槽已并入 `BR-03` AC1，其余路径登记 `DEFER-BR-03`。本编号不再拥有写集、验收标准或发布边界，不计入 35 张活动卡。

---

### Task BR-39: 已撤回（不得开工）

**Status:** withdrawn。原为 `REL-BR-03` 颜色家族唯一发布点；家族于 2026-11-18 撤回（见 `ADR-BR-02`），`BR-03` 改为 `independent` 自行完成 C/D。本编号不再拥有写集、验收标准或发布边界，不计入 35 张活动卡。

---

### Task BR-33: 已撤回（不得开工）

**Status:** superseded。基线 `src/command/branch.rs:3324-3339` 的 `list_name_column_width` 已按最长分支名计算列宽，`branch.rs:2933-2937` 已在 `-v`/`-vv` 使用该宽度。没有剩余实现缺口。本编号不再拥有写集、验收标准或发布边界，不计入 35 张活动卡。`BR-35` 同样已撤回。着色回归留在 `BR-03`，且该卡不得改动上述填充。

---

### Task BR-04: 修复 `--set-upstream-to` 支持 `refs/heads/<branch>` 并核对自追踪警告（#681）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #681：`resolve_upstream_spec` 支持 `refs/heads/<branch>` 作为上游本地分支引用，并确保自追踪时输出 `not setting branch '<b>' as its own upstream` 警告而不写入配置。
**Out of scope:** `--unset-upstream` 无上游报错（由 `BR-05` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `resolve_upstream_spec` 仅剥离 `refs/remotes/`，未剥离 `refs/heads/` | `src/command/branch.rs:1400-1432` |
| `set_upstream_impl` 自追踪分支检查需覆盖 `refs/heads/<self>` 入口 | `src/command/branch.rs:1507-1510` |
**Acceptance criteria:**
- [ ] 当前分支 `main` 上 `libra branch --set-upstream-to=refs/heads/other` 后 `branch.main.remote` 的值为 `.`（执行条件：退出码 0）。
- [ ] 同一命令后 `branch.main.merge` 的值为 `refs/heads/other`。
- [ ] `libra branch --set-upstream-to=refs/heads/main`（指向自身）的 stderr 含 `warning: not setting branch 'main' as its own upstream`（执行条件：退出码 0）（pinned Git 实测同为退出 0 + 同文案）。
- [ ] `libra branch --set-upstream-to=main`（短名指向自身）的 stderr 含同一警告（执行条件：退出码 0）。
- [ ] AC3 命令执行前后 SNAP 相等（不写入任何配置，预置值保持）。
- [ ] AC4 命令执行前后 SNAP 相等。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；分支 `main`（当前）与 `other`。AC3–AC6 在先以 `libra config branch.main.remote origin`、`libra config branch.main.merge refs/heads/keep` 预置配置的仓库执行；AC1、AC2 另建同形仓库（无预置）。
- AC1、AC2 → `command::branch_test::set_upstream_refs_heads`：运行命令后以 `libra config get branch.main.remote` / `libra config get branch.main.merge` 读取；判据分别等于 `.` 与 `refs/heads/other`。
- AC3、AC4 → 同一用例：判据 stderr 含该警告原文。
- AC5、AC6 → 同一用例：对应命令前后各取 SNAP；判据两次 SNAP 相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::set_upstream_refs_heads)'`（卡特有用例，new；覆盖 AC1–AC6）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-03, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--set-upstream-to支持refs/heads/与自追踪警告; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-03,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-03; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-05: 修复 `--unset-upstream` 在分支无上游配置时报错（#682）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #682：当目标分支未配置上游时，`libra branch --unset-upstream [<branch>]` 失败退出 128 并输出小写 `branch '<name>' has no upstream information`，与 Git 对齐。
**Out of scope:** `--set-upstream-to`（由 `BR-04` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `unset_upstream_impl` 直接调用 `ConfigKv::unset_all_with_conn` 而不检查是否存在 `branch.<b>.remote`/`merge` | `src/command/branch.rs:1675-1687` |
**Acceptance criteria:**
- [ ] 当前分支 `main` 无 `branch.main.remote` 与 `branch.main.merge` 时，`libra branch --unset-upstream` 的退出码为 128（`#682` 与 pinned Git 一致）。
- [ ] 上一条命令的 stderr 含 `branch 'main' has no upstream information`。
- [ ] 分支 `other` 无上游配置时，`libra branch --unset-upstream other` 的 stderr 含 `branch 'other' has no upstream information`（执行条件：以非零状态退出）。
- [ ] AC1 命令执行前后 SNAP 相等。
- [ ] AC3 命令执行前后 SNAP 相等。
- [ ] `main` 预置 `branch.main.remote=origin`、`branch.main.merge=refs/heads/main`、`branch.main.description=keep` 时，`libra branch --unset-upstream` 后 `config_kv` 中键名属于 `{branch.main.remote, branch.main.merge}` 的行数为 0（执行条件：退出码 0）。
- [ ] 上一条命令执行前后 SNAP 仅差这两行（`branch.main.description` 等其余行不变）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；分支 `main`（当前）与 `other`；AC6、AC7 另建仓库并按 AC 预置三项配置。
- AC1–AC5 → `command::branch_test::unset_upstream_without_config`：AC1 判据 `status.code() == Some(128)`；AC2、AC3 判据 stderr 含该文案；AC4、AC5 判据对应命令前后 SNAP 相等。
- AC6、AC7 → `command::branch_test::unset_upstream_with_config_succeeds`：命令前后各取 SNAP；AC6 判据所述行数为 0；AC7 判据两次 SNAP 删除这两行后逐字段相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::unset_upstream_without_config)'`（卡特有用例，new；覆盖 AC1–AC5）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::unset_upstream_with_config_succeeds)'`（卡特有用例，new；覆盖 AC6、AC7）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改 `docs/error-codes.md`
**Dependencies:** BR-04, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `docs/error-codes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `docs/error-codes.md`
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md` 与 `docs/error-codes.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--unset-upstream无上游报错; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-04,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-04; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-06: 支持 `@{upstream}` 核心修订解析与分支名解释（#684 部分 1/9）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #684 中 `@{upstream}` 核心解析轴：在 `src/utils/util.rs` 提供提交解析（`resolve_revision_expression_typed`）与分支名解释 helper，支持 `@{upstream}` 与 `<branch>@{upstream}`，接入 `src/command/branch.rs` 分支名参数路径（`branch -f @{upstream} two`），并在指向远程追踪分支或无上游配置时 fail-closed 拒绝。
**Out of scope:** `@{u}` 短写与大小写变体 `@{UPSTREAM}`/`@{U}`（拆出 `BR-32`）；detached HEAD `@{upstream}` 隐式拒绝与显式 `<branch>@{upstream}` 允许规则（拆出 `BR-31`）；`@{push}` 轴（拆出 `BR-25`/`29`/`27`/`30`/`26`/`28`）；`@{-N}` 与 `@{-1}@{upstream}` 链式语法（由 `BR-07` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `resolve_revision_expression_typed` 拒绝非数字 `@{...}` 且仅返回 `ObjectHash`，无法用于 `branch -f @{upstream}` 等需分支名的场景 | `src/utils/util.rs:2635-2675`、`src/command/branch.rs` |
**Acceptance criteria:**
- [ ] `main` 的上游为本地分支 `up` 时，在 `main` 上 `libra rev-parse @{upstream}` 的 stdout 为 `up` 的提交 ID（执行条件：退出码 0）。
- [ ] 在 `two` 上 `libra rev-parse main@{upstream}` 的 stdout 为 `up` 的提交 ID（执行条件：退出码 0）。
- [ ] 在 `main` 上 `libra branch -f @{upstream} two` 后 `libra rev-parse up` 输出 `two` 的提交 ID（`@{upstream}` 按分支名解释为 `up`）（执行条件：退出码 0）。
- [ ] `main` 的上游为远程追踪分支（`branch.main.remote=origin`、`branch.main.merge=refs/heads/x`，FR 生成 `refs/remotes/origin/x`）时，`libra branch -f @{upstream} two` 执行前后 SNAP 相等（执行条件：以非零状态退出）（不更新任何 ref；`t3204#4`，pinned Git 实测 exit 128 + `'@{upstream}' is not a valid branch name`）。
- [ ] `main` 无上游配置时 `libra rev-parse @{upstream}` 的退出码为 128（pinned Git 一致）。
- [ ] 上一条命令的 stderr 含 `no upstream configured for branch 'main'`（pinned Git 实测同文案）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具 FU：F0；`main` 上提交 `c1`、`c2`；分支 `up` 在 `c1`、`two` 在 `c2`；`main` 的上游为本地分支 `up`（`libra config branch.main.remote .`、`libra config branch.main.merge refs/heads/up`）。每次 `branch -f` 前以 `libra branch -f up <c1>` 复位 `up`（前置：`libra rev-parse up` 输出 `c1`）。
- AC1、AC2、AC5、AC6 → `command::rev_parse_test::upstream_revision`：AC1 在 `main` 上、AC2 先 `libra switch two`；判据 stdout 去尾换行后等于 `c1`。AC5、AC6 另建 `main` 无上游的仓库；AC5 判据 `status.code() == Some(128)`，AC6 判据 stderr 含该文案。
- AC3、AC4 → `command::branch_test::branch_upstream_name_interpretation`：AC3 判据 `libra rev-parse up` 输出 `c2`；AC4 按 AC 改配远程上游后取 SNAP、运行命令、再取 SNAP，判据两次 SNAP 相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::upstream_revision)'`（卡特有用例，new；覆盖 AC1、AC2、AC5、AC6）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::branch_upstream_name_interpretation)'`（卡特有用例，new；覆盖 AC3、AC4）
**Full-suite trigger:** T-1: 修改共享修订解析 helper `src/utils/util.rs`
**Dependencies:** BR-05, DEP-BR-08, DEP-BR-09, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `src/command/branch.rs`, `tests/command/rev_parse_test.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{upstream}核心修订与分支名解释; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-05,DEP-BR-08,DEP-BR-09,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-05; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-32: 支持 `@{u}` 短写与 `@{UPSTREAM}`/`@{U}` 大小写变体（#684 部分 2/9）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #684 中 `@{u}` 短写与大小写不敏感变体轴（自 `BR-06` 拆出）：在 `src/utils/util.rs` 与 `src/command/branch.rs` 补齐 `@{u}`、`<branch>@{u}` 及大小写变体 `@{UPSTREAM}`、`@{U}` 的修订解析与分支名参数解释（按 Git `object-name.c` 规则）。
**Out of scope:** `@{upstream}` 基础解析（已在 `BR-06` 交付）；detached HEAD 规则（拆出 `BR-31`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `util.rs` 与 `branch.rs` 需对齐 Git `object-name.c` 支持 `@{u}` 短写及 `@{UPSTREAM}`/`@{U}` 大小写变体 | `src/utils/util.rs:2635-2675`、`src/command/branch.rs` |
**Acceptance criteria:**
- [ ] 在 `main` 上 `libra rev-parse @{u}` 的 stdout 为 `up` 的提交 ID（执行条件：退出码 0）。
- [ ] 在 `two` 上 `libra rev-parse main@{u}` 的 stdout 为 `up` 的提交 ID（执行条件：退出码 0）。
- [ ] `libra rev-parse @{UPSTREAM}` 的 stdout 为 `up` 的提交 ID（后缀大小写不敏感，Git `object-name.c` 以 `strncasecmp` 比较）（执行条件：退出码 0）。
- [ ] `libra rev-parse @{U}` 的 stdout 为 `up` 的提交 ID（执行条件：退出码 0）。
- [ ] `libra branch -f @{u} two` 后 `libra rev-parse up` 输出 `two` 的提交 ID（执行条件：退出码 0）。
- [ ] `libra branch -f @{UPSTREAM} two` 后 `libra rev-parse up` 输出 `two` 的提交 ID（执行条件：退出码 0）。
- [ ] `libra branch -f @{U} two` 后 `libra rev-parse up` 输出 `two` 的提交 ID（执行条件：退出码 0）。
- [ ] 在 `two`（其上游为 `side`）上 `libra branch -f main@{u} three` 后 `libra rev-parse up` 输出 `three` 的提交 ID（显式 `<branch>@{u}` 按指定分支而非当前分支解释）（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具 FU：F0；`main` 上提交 `c1`、`c2`；分支 `up` 在 `c1`、`two` 在 `c2`；`main` 的上游为本地分支 `up`（`libra config branch.main.remote .`、`libra config branch.main.merge refs/heads/up`）。每次 `branch -f` 前以 `libra branch -f up <c1>` 复位 `up`（前置：`libra rev-parse up` 输出 `c1`）。AC8 另建 `side` 在 `c1`，并给 `two` 预置上游 `branch.two.remote=.`、`branch.two.merge=refs/heads/side`，再提交 `c3` 到分支 `three`。
- AC1–AC4 → `command::rev_parse_test::upstream_short_and_case_variants`：AC2 先 `libra switch two`，其余在 `main` 上；判据 stdout 等于 `c1`。
- AC5–AC8 → `command::branch_test::branch_upstream_short_and_case_name`：每条前复位 `up`；AC5–AC7 判据 `libra rev-parse up` 输出 `c2`；AC8 在 `two` 上执行，判据输出 `c3`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::upstream_short_and_case_variants)'`（卡特有用例，new；覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::branch_upstream_short_and_case_name)'`（卡特有用例，new；覆盖 AC5–AC8）
**Full-suite trigger:** T-1: 修改共享修订解析 helper `src/utils/util.rs`
**Dependencies:** BR-06, DEP-BR-08, DEP-BR-09, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `src/command/branch.rs`, `tests/command/rev_parse_test.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/branch.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{u}与@{UPSTREAM}/@{U}大小写变体; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-06,DEP-BR-08,DEP-BR-09,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-06; release=independent; split-from=BR-06; exception=N/A`

---

### Task BR-31: 完善 `@{upstream}` / `@{u}` detached HEAD 规则（#684 部分 3/9）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #684 中 `@{upstream}`/`@{u}` detached HEAD 规则轴（自 `BR-06` 拆出）：区分从 `HEAD` 推断分支的隐式 `@{upstream}`/`@{u}`（detached HEAD 下 fail-closed 报错）与显式 `<branch>@{upstream}`/`<branch>@{u}`（detached HEAD 下仍按 `<branch>` 配置解析并可用于 `branch -f`）。
**Out of scope:** `@{upstream}` 基础解析与本地/远程 `branch -f`（已在 `BR-06` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `util.rs` 与 `branch.rs` 需区分 detached HEAD 下隐式 `@{upstream}` 拒绝与显式 `<branch>@{upstream}` 允许 | `src/utils/util.rs:2635-2675`、`src/command/branch.rs` |
**Acceptance criteria:**
- [ ] detached HEAD 下 `libra rev-parse @{upstream}` 的 stderr 含 `HEAD does not point to a branch`（执行条件：以非零状态退出）（pinned Git 实测 exit 128 + 同文案）。
- [ ] detached HEAD 下 `libra rev-parse @{u}` 的 stderr 含 `HEAD does not point to a branch`（执行条件：以非零状态退出）。
- [ ] detached HEAD 下 `libra rev-parse main@{upstream}` 的 stdout 为 `up` 的提交 ID（执行条件：退出码 0）。
- [ ] detached HEAD 下 `libra rev-parse main@{u}` 的 stdout 为 `up` 的提交 ID（执行条件：退出码 0）。
- [ ] detached HEAD 下 `libra branch -f main@{upstream} two` 后 `libra rev-parse up` 输出 `two` 的提交 ID（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具 FU：F0；`main` 上提交 `c1`、`c2`；分支 `up` 在 `c1`、`two` 在 `c2`；`main` 的上游为本地分支 `up`（`libra config branch.main.remote .`、`libra config branch.main.merge refs/heads/up`）。每次 `branch -f` 前以 `libra branch -f up <c1>` 复位 `up`（前置：`libra rev-parse up` 输出 `c1`）。
- 进入 detached HEAD 后前置：`libra rev-parse --abbrev-ref HEAD` 输出 `HEAD`。
- AC1–AC4 → `command::rev_parse_test::upstream_detached_head`：AC1、AC2 判据 stderr 含该文案；AC3、AC4 判据 stdout 等于 `c1`。
- AC5 → `command::branch_test::branch_upstream_detached_explicit`：判据 `libra rev-parse up` 输出 `c2`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::upstream_detached_head)'`（卡特有用例，new；覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::branch_upstream_detached_explicit)'`（卡特有用例，new；覆盖 AC5）
**Full-suite trigger:** T-1: 修改共享修订解析 helper `src/utils/util.rs`
**Dependencies:** BR-32, DEP-BR-08, DEP-BR-09, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `src/command/branch.rs`, `tests/command/rev_parse_test.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/branch.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{upstream}detachedHEAD规则; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-32,DEP-BR-08,DEP-BR-09,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-32; release=independent; split-from=BR-06; exception=N/A`

---

### Task BR-07: 支持 `@{-N}` 与 `@{-1}@{upstream}` 修订与分支名解释（#685）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #685：在 `src/utils/util.rs` 的修订解析与分支名解释 helper 中支持 `@{-N}`（`N >= 1`）与链式 `@{-1}@{upstream}` / `@{-1}~1`，并接入 `src/command/branch.rs` 的 `--set-upstream-to`/`--unset-upstream` 分支名参数路径（`#685` 验收项）；创建与 `-f` 路径的 `@{-N}` 分支名解释不在本卡（`DEFER-BR-07`）。
**Out of scope:** `@{upstream}`/`@{u}` 基础解析（由 `BR-06`/`BR-31` 交付）；`@{push}`（由 `BR-25` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `resolve_revision_expression_typed` 拒绝 `-1` 等非纯数字索引，且 `branch` 的 `-u`/`--unset-upstream` 未通过分支名解释器解析 `@{-1}` | `src/utils/util.rs:2652-2656`、`src/command/branch.rs:1495-1687` |
**Acceptance criteria:**
- [ ] 检出历史为 `main` → `third` → `other` → `main` 时，`libra rev-parse @{-1}` 的 stdout 为 `other` 的提交 ID（`o1`）（执行条件：退出码 0）。
- [ ] 同一历史下 `libra rev-parse @{-2}` 的 stdout 为 `third` 的提交 ID（执行条件：退出码 0）。
- [ ] `libra rev-parse @{-1}@{upstream}` 的 stdout 为 `other` 的上游 `base` 的提交 ID（执行条件：退出码 0）。
- [ ] `libra rev-parse @{-1}~1` 的 stdout 为 `other` 提交的父提交 ID（执行条件：退出码 0）。
- [ ] 仓库 Y 中 `libra branch --set-upstream-to base @{-1}` 后 `branch.other.merge` 的值为 `refs/heads/base`（`#685` Acceptance 1）（执行条件：退出码 0）。
- [ ] 随后 `libra branch --unset-upstream @{-1}@{upstream}` 后 `branch.base.merge` 不存在（`t3204#16`；`#685` 文案里的 `branch.upstream-branch` 是笔误）（执行条件：退出码 0）。
- [ ] 检出历史不足 5 次时 `libra rev-parse @{-5}` 的退出码为 128（pinned Git 实测 128）。
- [ ] `libra rev-parse @{-0}` 的退出码为 128（pinned Git 实测 128）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 仓库 X（AC1–AC4、AC7、AC8）：F0；`main` 上提交 `c1`、`c2`（`main` 在 `c2`）；分支 `base` 在 `c1`；`third` 自 `c1` 新增提交 `t1`；`other` 自 `c1` 新增提交 `o0`、`o1`（在 `o1`），预置上游 `branch.other.remote=.`、`branch.other.merge=refs/heads/base`；依次 `libra switch third`、`libra switch other`、`libra switch main`。五个被比较的提交 `c1`、`c2`、`t1`、`o0`、`o1` 两两不同。
- 仓库 Y（AC5、AC6，按 `t3204#16` 形态）：同形但 `other` 不预置上游，`base` 预置上游 `branch.base.remote=.`、`branch.base.merge=refs/heads/main`；同样依次 `switch third`、`switch other`、`switch main`；AC5 先于 AC6。
- AC1–AC4、AC7、AC8 → `command::rev_parse_test::previous_branch_nth_revision`：AC1 判据 stdout 为 `o1`；AC2 判据 stdout 为 `t1`；AC3 判据 stdout 为 `c1`；AC4 判据 stdout 为 `o0`；AC7、AC8 判据 `status.code() == Some(128)`。
- AC5、AC6 → `command::branch_test::branch_previous_branch_upstream`：AC5 判据 `libra config get branch.other.merge` 输出 `refs/heads/base`；AC6 判据经 SNAP 的同一连接读取 `config_kv` 中键为 `branch.base.merge` 的行数为 0。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::previous_branch_nth_revision)'`（卡特有用例，new；覆盖 AC1–AC4、AC7、AC8）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::branch_previous_branch_upstream)'`（卡特有用例，new；覆盖 AC5、AC6）
**Full-suite trigger:** T-1: 修改共享修订解析 helper `src/utils/util.rs`
**Dependencies:** BR-31, DEP-BR-08, DEP-BR-09, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `src/command/branch.rs`, `tests/command/rev_parse_test.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{-N}与链式@{upstream}修订与分支名解释; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-31,DEP-BR-08,DEP-BR-09,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-31; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-08: 修复 `libra branch -m` 无参数退出码为 128 并提示 `branch name required`（#687）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #687：将 `libra branch -m` 在无参数时的处理从 clap 的 129 参数缺失改为执行期 `branch name required` 报错并退出 128，对齐 Git。
**Out of scope:** `-m` 迁移 reflog（由 `BR-09` 承接）；`-c`/`-C` 缺参处理（仅在显式需要时另开卡）；超过 2 个参数的退出码与文案（`DEFER-BR-05`，本卡不作断言）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `BranchArgs.rename` 声明 `num_args = 1..=2`，导致 `libra branch -m` 直接在 clap 解析层退出 129 | `src/command/branch.rs:429-430,2247-2255` |
**Acceptance criteria:**
- [ ] `libra branch -m`（0 个参数）的退出码为 128（`#687` Acceptance 1）。
- [ ] 上一条命令的 stderr 含 `branch name required`（`#687` Acceptance 2）。
- [ ] 在 `topic` 上 `libra branch -m renamed`（1 个参数）后 `libra branch --show-current` 输出 `renamed`（执行条件：退出码 0）。
- [ ] 上一条重命名后 `libra rev-parse --verify refs/heads/topic` 以非零状态退出（旧名不存在）。
- [ ] `libra branch -m other moved`（2 个参数）后 `libra rev-parse moved` 输出执行前 `other` 的提交 ID（执行条件：退出码 0）。
- [ ] 上一条重命名后 `libra rev-parse --verify refs/heads/other` 以非零状态退出。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；分支 `main`、`topic`（当前）、`other`（与 `topic` 不同提交）。
- AC1、AC2 → `command::branch_test::rename_missing_arg_exit_128`：AC1 判据 `status.code() == Some(128)`；AC2 判据 stderr 含该文案。
- AC3–AC6 → `command::branch_test::rename_one_and_two_arg_behaviour`：AC3 判据 `--show-current` 输出 `renamed`；AC4、AC6 判据对应 `rev-parse --verify` 的退出码 ≠ 0；AC5 判据输出等于执行前记录的 `other` 提交 ID。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::rename_missing_arg_exit_128)'`（卡特有用例，new；覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::rename_one_and_two_arg_behaviour)'`（卡特有用例，new；覆盖 AC3–AC6）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改 `docs/error-codes.md`
**Dependencies:** BR-07, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `docs/error-codes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md` 与 `docs/error-codes.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-m缺参退出码128与提示; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-07,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-07; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-09: 修复 `libra branch -m` 迁移源分支 reflog 至新分支（#688）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #688：`rename_branch_impl` 在重命名分支时将 `refs/heads/<old>` 的 reflog 完整迁移（并追加重命名记录）到 `refs/heads/<new>`，移除旧 reflog，使 `libra reflog exists <new>` 通过且 `<old>` 不再残留。
**Out of scope:** `branch --copy` 复制 reflog（由 `BR-11` 承接）；`-M` 强制重命名及其覆盖目标时的迁移与恢复（`DEFER-BR-04`，`#675`；`BranchArgs` 现无 `-M`）；加密（`encrypted = 1`）配置行的迁移（`DEFER-BR-09`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `rename_branch_impl` 仅更新 ref、HEAD 与 `MetadataKv`，未迁移 `src/internal/reflog.rs` 中的分支 reflog | `src/command/branch.rs:2282-2316` |
**Acceptance criteria:**
- [ ] `libra branch -m topic renamed` 后 `libra reflog exists refs/heads/renamed` 退出 0（`#688` Acceptance 2）。
- [ ] `renamed` 的 reflog 去掉末条后，按顺序比较 `(old_oid, new_oid, committer_name, committer_email, timestamp, action, message, worktree_id)` 等于 `topic` 执行前的全部 reflog 条目（只有 `id` 与 `ref_name` 允许变化）。
- [ ] `renamed` 的 reflog 末条 `message` 为 `Branch: renamed refs/heads/topic to refs/heads/renamed`（Git `builtin/branch.c:672`）。
- [ ] 重命名后 `libra reflog exists refs/heads/topic` 以非零状态退出（旧 reflog 已移除）。
- [ ] 重命名后 `config_kv` 中非加密（`encrypted = 0`）且键名以 `branch.topic.` 或 `branch.renamed.` 开头的行的（键, `value`, `encrypted`）按 `id` 排序的序列，等于执行前非加密 `branch.topic.*` 行的同一序列把前缀改为 `branch.renamed.` 的结果（整段迁移含自定义键与同键多值、保持次序与重复，旧名配置清理）。
- [ ] 重命名后 `reference` 表中 `kind='Branch'` 的（name, remote, commit）集合，等于执行前该集合把 `topic` 改名为 `renamed` 的结果（新 ref 指向原提交、旧 ref 不存在、其它分支不变）。
- [ ] 重命名后 `libra branch --show-current` 输出 `renamed`（当前分支随重命名移动）（执行条件：退出码 0）。
- [ ] FI 使重命名在写入 `refs/heads/renamed` 的 reflog 时失败：`libra branch -m topic renamed` 执行前后 SNAP 相等（执行条件：以非零状态退出，且 stderr 含注入标识 `br-fi`，证明失败发生在注入点——`branch_config_write_error` 等错误包装保留底层 SQLite 消息，`src/command/branch.rs:1346-1350`）（ref、HEAD、metadata、config、reflog 全部保持操作前状态）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；`topic`（当前分支）在 `main` 之上有 2 个提交（reflog 至少 3 条），预置 `branch.topic.remote=origin`、`branch.topic.merge=refs/heads/topic`、`branch.topic.description=keep`、`branch.topic.custom-key=x`（自定义键，区分只迁移固定键的实现）与一条 `metadata_kv` 分支元数据，以及以 `libra config set --add` 依次写入的同键多值 `branch.topic.multi=a`、`b`、`a`（含重复值，区分合并重复或改变顺序的实现；夹具全部为非加密行，加密行见 `DEFER-BR-09`）；前置：执行前不存在任何 `branch.renamed.*` 行（目标名已有孤立配置时 Git 保留孤立段，pinned Git 实测，属 `DEFER-BR-10`）；执行前记录 SNAP 中 `ref_name='refs/heads/topic'` 的 reflog 行与 `branch.topic.*` 行。
- AC1–AC7 → `command::branch_test::rename_moves_reflog`：AC1、AC4 判据对应 `reflog exists` 的退出码；AC2 判据 `ref_name='refs/heads/renamed'` 的行（按 `id` 排序）去掉末条后与执行前记录逐条比较所列八字段相等；AC3 判据末条 `message` 等于该文案；AC5 判据所述序列等式成立；AC6 判据所述集合等式成立；AC7 判据输出 `renamed`。
- AC8 → `command::branch_test::rename_partial_failure_rollback`：同形夹具安装 FI（`BEFORE INSERT ON reflog WHEN NEW.ref_name = 'refs/heads/renamed'`）后取 SNAP，运行命令，再取 SNAP；判据两次 SNAP 相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::rename_moves_reflog)'`（卡特有用例，new；覆盖 AC1–AC7）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::rename_partial_failure_rollback)'`（卡特有用例，new；覆盖 AC8）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^internal::reflog::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享 reflog 存储模块 `src/internal/reflog.rs`
**Dependencies:** BR-08, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/internal/reflog.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-m重命名迁移reflog; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-08,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-08; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-10: 修复创建分支时的 D/F ref 冲突检查与错误点名（#689）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #689 的创建轴。当 `libra branch <name>` 的目标名与已有 ref 构成目录/文件冲突时，命令退出非零，并且 stderr 点名被碰撞的 ref。本卡拆出 `BR-34`（重命名）和 `BR-36`（复制）。创建轴的检查和错误文本在本卡一起交付，并独立 `patch + 1`。
**Out of scope:** 重命名冲突（`BR-34`）；复制冲突（`BR-36`）；同名分支自身原地复制（`BR-12`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `rename_branch_impl` 与 `Branch::update_branch` 仅做精确同名检查，未检查前缀/子路径冲突 | `src/command/branch.rs:2274-2280`、`src/internal/branch.rs` |
**Acceptance criteria:**
- [ ] 存在分支 `o/p` 时 `libra branch o` 退出 128（pinned Git 实测：exit 128，stderr 含 `'refs/heads/o/p' exists; cannot create 'refs/heads/o'`）。
- [ ] 上一条失败的 stderr 含 `'refs/heads/o/p' exists`（点名被碰撞的 ref）。
- [ ] 存在分支 `o` 时 `libra branch o/p` 退出 128。
- [ ] 上一条失败的 stderr 含 `'refs/heads/o' exists`。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；仓库 A 建分支 `o/p`；仓库 B 建分支 `o`。
- AC1、AC2 → `command::branch_test::ref_namespace_df_conflict_create`：在仓库 A 运行 `libra branch o`；AC1 判据 exit = 128，AC2 判据 stderr 含该片段。
- AC3、AC4 → 同一用例：在仓库 B 运行 `libra branch o/p`；AC3 判据 exit = 128，AC4 判据 stderr 含该片段。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::ref_namespace_df_conflict_create)'`（卡特有用例，new；覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^internal::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享 helper `src/internal/branch.rs`
**Dependencies:** BR-09, DEP-BR-08, DEP-BR-11, DEP-BR-12, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/internal/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=创建分支D/F冲突检查与错误点名; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=BR-09,DEP-BR-08,DEP-BR-11,DEP-BR-12,DEP-BR-14; writeset=序列化于BR-09; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-34: 修复重命名时的 D/F ref 冲突检查、错误点名与源排除（#689）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 自 `BR-10` 拆出（G-09）。本卡只交付重命名轴：冲突时退出非零且 stderr 点名被碰撞的 ref，并且 `libra branch -m m m/m` 退出 0（源路径不把自己算成冲突）。本卡拆出 `BR-36`。本卡独立 `patch + 1`。
**Out of scope:** 创建冲突（`BR-10`）；复制冲突（`BR-36`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| 冲突错误尚未稳定点名被碰撞的引用，源路径也未从冲突集排除 | `src/command/branch.rs:2274-2280` |
**Acceptance criteria:**
- [ ] 存在分支 `o/o` 与 `o/p` 时 `libra branch -m o/o o` 的退出码为 128（源 `o/o` 不计为冲突，`o/p` 仍冲突；pinned Git 实测 128）。
- [ ] 上一条命令的 stderr 含 `'refs/heads/o/p' exists`（执行条件：以非零状态退出）。
- [ ] 存在分支 `o` 与 `x` 时 `libra branch -m x o/p` 的退出码为 128。
- [ ] 上一条命令的 stderr 含 `'refs/heads/o' exists`（执行条件：以非零状态退出）。
- [ ] 存在分支 `m` 时 `libra branch -m m m/m` 后 `libra rev-parse m/m` 输出执行前 `m` 的提交 ID（源路径不把自己算成冲突；pinned Git 实测 exit 0）（执行条件：退出码 0）。
- [ ] 上一条重命名后 `libra rev-parse --verify refs/heads/m` 以非零状态退出（旧名已移走）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；仓库 A 建 `o/o`、`o/p`；仓库 B 建 `o`、`x`；仓库 C 建 `m`（记录其提交 ID）。
- AC1–AC6 → `command::branch_test::ref_namespace_df_conflict_rename`：AC1、AC3 判据 `status.code() == Some(128)`；AC2、AC4 判据 stderr 含该片段；AC5 判据输出等于记录值；AC6 判据 `rev-parse --verify` 的退出码 ≠ 0。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::ref_namespace_df_conflict_rename)'`（卡特有用例，new；覆盖 AC1–AC6）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^internal::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享 helper `src/internal/branch.rs`
**Dependencies:** BR-10, DEP-BR-08, DEP-BR-11, DEP-BR-12, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/internal/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=重命名D/F冲突检查与错误点名; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=BR-10,DEP-BR-08,DEP-BR-11,DEP-BR-12,DEP-BR-14; writeset=序列化于BR-10; release=independent; split-from=BR-10; exception=N/A`

---

### Task BR-35: 已撤回（不得开工）

**Status:** superseded。Codex R39 判定 `REL-BR-10` 没有不可分割的中间态。创建、重命名、复制已分别由 `BR-10`、`BR-34`、`BR-36` 独立发布。本编号不再拥有写集、验收标准或发布边界，不计入 35 张活动卡。

---

### Task BR-36: 修复复制时的 D/F ref 冲突检查与错误点名（#689）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 自 `BR-34` 拆出（G-09）。本卡只交付复制轴：`-c` 与 `-C` 在目标名构成 D/F 冲突时各自退出非零，并且 stderr 点名被碰撞的 ref。检查和错误文本在本卡一起交付，并独立 `patch + 1`。
**Out of scope:** 创建冲突（`BR-10`）；重命名冲突与 `m m/m`（`BR-34`）；reflog 与 `branch.*` 配置复制（`BR-11`）；同名原地复制（`BR-12`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| 复制路径尚未检查 D/F 前缀冲突 | `src/command/branch.rs:2274-2280`、`src/command/branch.rs:2344-2350` |
**Acceptance criteria:**
- [ ] 存在分支 `o/p` 时 `libra branch -c src o` 退出 128。
- [ ] 上一条失败的 stderr 含 `'refs/heads/o/p' exists`。
- [ ] 存在分支 `o` 时 `libra branch -c src o/p` 退出 128。
- [ ] 上一条失败的 stderr 含 `'refs/heads/o' exists`。
- [ ] 存在分支 `o/p` 时 `libra branch -C src o` 退出 128。
- [ ] 上一条失败的 stderr 含 `'refs/heads/o/p' exists`。
- [ ] 存在分支 `o` 时 `libra branch -C src o/p` 退出 128。
- [ ] 上一条失败的 stderr 含 `'refs/heads/o' exists`。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；仓库 A 建 `src`、`o/p`；仓库 B 建 `src`、`o`（pinned Git 对四条命令均实测 exit 128 + 对应片段）。
- AC1、AC2、AC5、AC6 → `command::branch_test::ref_namespace_df_conflict_copy`：仓库 A 分别运行 `-c` 与 `-C`；退出码类 AC 判据 exit = 128，点名类 AC 判据 stderr 含该片段。
- AC3、AC4、AC7、AC8 → 同一用例：仓库 B 分别运行 `-c` 与 `-C`；判据同上。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::ref_namespace_df_conflict_copy)'`（卡特有用例，new；覆盖 AC1–AC8）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^internal::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享 helper `src/internal/branch.rs`
**Dependencies:** BR-34, DEP-BR-08, DEP-BR-11, DEP-BR-12, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/internal/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=复制D/F冲突检查与错误点名; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=BR-34,DEP-BR-08,DEP-BR-11,DEP-BR-12,DEP-BR-14; writeset=序列化于BR-34; release=independent; split-from=BR-34; exception=N/A`

---

### Task BR-11: 修复 `libra branch --copy` 复制 reflog 与全量 `branch.*` 配置（#690）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #690：`copy_branch_impl` 在复制分支时同时复制源分支的 reflog（并追加 copy 记录）以及全部非加密 `branch.<old>.*` 配置项（含 `description`、`pushRemote`、`rebase` 与自定义键、同键多值，不仅限于 `remote`/`merge`；加密配置行见 `DEFER-BR-09`）。
**Out of scope:** 同名复制 `m2 m2`（由 `BR-12` 承接）；`-c -f` 参数校验（由 `BR-13` 承接）；`-C` 覆盖已有目标时的配置与 reflog 替换语义（`DEFER-BR-06`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `copy_branch_impl` 硬编码仅复制 `["remote", "merge"]`，且未复制 reflog | `src/command/branch.rs:2372-2384` |
**Acceptance criteria:**
- [ ] `libra branch -c topic copy` 后，`copy` 的 reflog 去掉末条后按顺序比较 `(old_oid, new_oid, committer_name, committer_email, timestamp, action, message, worktree_id)` 等于 `topic` 执行前的全部 reflog 条目（`#690`）（执行条件：退出码 0）。
- [ ] `copy` 的 reflog 末条 `message` 为 `Branch: copied refs/heads/topic to refs/heads/copy`（Git `builtin/branch.c:669`；pinned Git 实测）。
- [ ] `topic` 的 reflog 行执行前后逐字段相等。
- [ ] 非加密（`encrypted = 0`）`branch.copy.*` 行的（键, `value`, `encrypted`）按 `id` 排序的序列等于执行前非加密 `branch.topic.*` 行的同一序列把前缀改为 `branch.copy.` 的结果（整段复制，含 `#690` 的 `dummy`、自定义键与同键多值，保持次序与重复）。
- [ ] `branch.topic.*` 的行执行前后逐字段相等。
- [ ] 复制后 `reference` 表的全部行（含 HEAD 行）等于执行前全部行再加一行 `kind='Branch'`、`name='copy'`、`commit` 等于 `topic` 提交的分支行（源 ref 与 HEAD 不变，忽略新行 `id`）。
- [ ] FI 使复制在写入 `refs/heads/copy` 的 reflog 时失败：`libra branch -c topic copy` 执行前后 SNAP 相等（执行条件：以非零状态退出，且 stderr 含注入标识 `br-fi`，证明失败发生在注入点——`branch_config_write_error` 等错误包装保留底层 SQLite 消息，`src/command/branch.rs:1346-1350`）（不留新 ref、半复制的配置、metadata 或 reflog）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；`topic` 在 `main` 之上有 2 个提交（reflog 至少 3 条），预置 `branch.topic.remote`、`merge`、`description`、`rebase`、`pushRemote`、`dummy=Hello` 与自定义键 `branch.topic.custom-key=x`，一条 `metadata_kv` 分支元数据，以及以 `libra config set --add` 依次写入的同键多值 `branch.topic.multi=a`、`b`、`a`（含重复值，区分合并重复或改变顺序的实现；夹具全部为非加密行，加密行见 `DEFER-BR-09`）；前置：执行前不存在任何 `branch.copy.*` 行（孤立目标配置见 `DEFER-BR-10`）；执行前记录 `topic` 的 reflog 行与 `branch.topic.*` 行。
- AC1–AC6 → `command::branch_test::copy_preserves_reflog_and_all_config`：AC1 判据 `ref_name='refs/heads/copy'` 的行（按 `id` 排序）去掉末条后与执行前 `topic` 记录逐条比较所列八字段相等；AC2 判据末条 `message` 等于该文案；AC3、AC5 判据所述行与执行前记录逐字段相等；AC4 判据所述序列相等；AC6 判据 `reference` 表集合等式成立。
- AC7 → `command::branch_test::copy_partial_failure_rollback`：同形夹具安装 FI（`BEFORE INSERT ON reflog WHEN NEW.ref_name = 'refs/heads/copy'`）后取 SNAP，运行命令，再取 SNAP；判据两次 SNAP 相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::copy_preserves_reflog_and_all_config)'`（卡特有用例，new；覆盖 AC1–AC6）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::copy_partial_failure_rollback)'`（卡特有用例，new；覆盖 AC7）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^internal::reflog::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享 reflog 存储模块 `src/internal/reflog.rs`
**Dependencies:** BR-36, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/internal/reflog.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--copy复制reflog与全量非加密branch.*配置; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-36,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-36; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-12: 支持 `libra branch -c` 同名分支复制成功语义（#691）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #691：当源分支与目标分支名相同时（如 `libra branch -c m2 m2` 或在 `m2` 上执行 `libra branch -c m2`），允许成功返回：ref 与 `branch.m2.*` 配置不变、原 reflog 保留，并与 Git 一致追加一条 `Branch: copied refs/heads/m2 to refs/heads/m2` 记录（pinned Git 实测）。
**Out of scope:** `-c -f` 强制标志解析（由 `BR-13` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `copy_branch_impl` 在 `!force && destination_exists` 时无条件返回 `AlreadyExists` | `src/command/branch.rs:2344-2350` |
**Acceptance criteria:**
- [ ] `libra branch -c m2 m2` 退出 0（不报 `already exists`；`#691` Acceptance 1）。
- [ ] 当前分支为 `m2` 时 `libra branch -c m2` 退出 0。
- [ ] AC1 命令执行前后 SNAP 的 `reference`、`config_kv`、`config`、`metadata_kv` 四表相等（`m2` 提交与 `branch.m2.dummy=Hello` 不变）。
- [ ] AC2 命令执行前后上述四表相等。
- [ ] AC1 命令后 `m2` 的 reflog 行（按 `id`）等于执行前全部行（逐字段不变）之后恰追加一行，且该行 `message` 为 `Branch: copied refs/heads/m2 to refs/heads/m2`（pinned Git 实测同名复制同样追加该条）。
- [ ] AC2 命令后 `m2` 的 reflog 行等于执行前全部行（逐字段不变）之后恰追加一行，且该行 `message` 为 `Branch: copied refs/heads/m2 to refs/heads/m2`。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；`libra branch --create-reflog m2`、`libra config branch.m2.dummy Hello`；AC2 前 `libra switch m2`；每条命令前取 SNAP 与 `m2` 的 reflog 行。
- AC1–AC6 → `command::branch_test::copy_same_name_succeeds`：AC1、AC2 判据 `status.code() == Some(0)`；AC3、AC4 判据四表行逐字段相等；AC5、AC6 判据执行后 `ref_name='refs/heads/m2'` 的行数比执行前多 1、前缀逐字段等于执行前全部行、末行 `message` 等于该文案（按一条序列等式判定）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::copy_same_name_succeeds)'`（卡特有用例，new；覆盖 AC1–AC6）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-11, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-c同名复制成功语义; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-11,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-11; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-13: 支持 `libra branch -c -f` 强制复制（#692）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #692：修复 `BranchArgs.copy` 的 `num_args = 1..=2` 导致 `-c -f` 被 clap 提前报错（`a value is required for '--copy'`）的解析问题，并放开 `BranchArgs.force` 的 `requires = "new_branch"` 约束，使 `libra branch -c -f` / `--copy --force` 等价于 `-C`。
**Out of scope:** `-C` 自身语义（基线已支持）；`-M`（`DEFER-BR-04`）。`-f` 单独或与 `--list` 同用时按 Git 列出分支（见 AC3、AC4），不再拒绝。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `BranchArgs.copy` 声明 `num_args = 1..=2` 吞并/拒绝 `-c -f`，且 `force` 标记了 `requires = "new_branch"` | `src/command/branch.rs:348-349,435-436` |
**Acceptance criteria:**
- [ ] 已存在 `existing` 时，`libra branch -c -f topic existing` 执行后的 SNAP（除 `id` 与时间戳列 `reflog.timestamp`、`metadata_kv.created_at`/`updated_at` 外），等于在对照仓库上执行 `libra branch -C topic existing` 后的 SNAP（对照仓库为被测仓库在执行命令前的整目录字节副本，提交 ID 与各表初值相同；`-c -f` 与 `-C` 完全等价，含目标提交、复制的 `branch.*` 配置与 metadata；`#692` Acceptance 1）（执行条件：退出码 0）。
- [ ] `libra branch --copy --force topic existing` 执行后的 SNAP（除 `id` 与时间戳列 `reflog.timestamp`、`metadata_kv.created_at`/`updated_at` 外），等于在对照仓库（被测仓库执行命令前的整目录字节副本）上执行 `libra branch -C topic existing` 后的 SNAP（执行条件：退出码 0）。
- [ ] `libra branch -f`（无其它参数）的 stdout 与 `libra branch` 的 stdout 字节相同（`-f` 对列表无效果；pinned Git 实测 `git branch -f` 列出分支并退出 0）（执行条件：退出码 0）。
- [ ] `libra branch --list -f` 的 stdout 与 `libra branch --list` 的 stdout 字节相同（pinned Git 实测同样列出分支）（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；`main` 上提交 `c1`、`c2`；`topic` 在 `c2`，预置 `branch.topic.remote=origin`、`branch.topic.merge=refs/heads/topic` 与一条 `metadata_kv` 分支元数据；`existing` 在 `c1`。AC1、AC2 各在建好夹具后把被测仓库整目录复制为对照仓库（字节相同，避免分别建库导致提交 ID 不同），被测仓库执行被测命令、对照仓库执行 `-C`。
- AC1–AC4 → `command::branch_test::copy_with_force_flag`：AC1、AC2 判据两仓库 SNAP 去掉所列列后逐字段相等；AC3、AC4 判据与对照命令 stdout 字节相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::copy_with_force_flag)'`（卡特有用例，new；覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-12, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-c-f强制复制; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-12,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-12; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-14: 为 `--ignore-case` 添加短选项 `-i`（#693）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #693：在 `BranchArgs.ignore_case` 上添加 `short = 'i'`，支持 `libra branch -i` 进行大小写不敏感的分支排序与模式过滤。
**Out of scope:** `--list <pattern>` 实现（已在 `BR-02` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `BranchArgs.ignore_case` 仅有 `long = "ignore-case"`，缺少 `short = 'i'` | `src/command/branch.rs:480-482` |
**Acceptance criteria:**
- [ ] `libra branch -i --list 'a*'` 的 stdout 恰为 `"  a\n  Alpha\n"`（按忽略大小写的顺序；pinned Git 实测 `git branch -i --list 'a*'` 依次输出 `a`、`Alpha`）（执行条件：退出码 0）。
- [ ] `libra branch -i` 的 stdout 与 `libra branch --ignore-case` 的 stdout 字节相同（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；分支 `main`、`a`、`Alpha`、`beta`。
- AC1、AC2 → `command::branch_test::short_ignore_case_flag`：AC1 判据 stdout 字节相等（区分大小写的错误排序会得到 `Alpha` 在前）；AC2 判据两次 stdout 字节相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::short_ignore_case_flag)'`（卡特有用例，new；覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-13, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-i短选项别名; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-13,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-13; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-15: 修复 `--show-current` 在 detached HEAD 时保持静默（#694）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #694：当 `HEAD` 处于 detached 状态时，`libra branch --show-current` 不输出任何文本（stdout 为空）并退出 0，与 Git 对齐。
**Out of scope:** `--json` 模式下 `BranchOutput::ShowCurrent` 结构字段（保持 JSON 结构兼容）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `render_branch_output` 在 `BranchOutput::ShowCurrent { detached: true, .. }` 时打印 `HEAD detached at ...` | `src/command/branch.rs:3012-3024` |
**Acceptance criteria:**
- [ ] detached HEAD 下 `libra branch --show-current` 的 stdout 为空（`#694`）（执行条件：退出码 0）。
- [ ] 同一命令的 stderr 为空。
- [ ] 附着分支 `main` 上 `libra branch --show-current` 的 stdout 恰为 `main\n`（执行条件：退出码 0）。
- [ ] 未出生分支（`libra init` 后无提交，HEAD 指向 `main`）上 `libra branch --show-current` 的 stdout 恰为 `main\n`（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；仓库 A 有一个提交（AC1、AC2 进入 detached HEAD，前置 `libra rev-parse --abbrev-ref HEAD` 输出 `HEAD`；AC3 在 `main` 上）；仓库 B 只 `libra init`。
- AC1–AC4 → `command::branch_test::show_current_detached_silent`：判据 stdout/stderr 字节等于 AC 所述。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::show_current_detached_silent)'`（卡特有用例，new；覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-14, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--show-current在detachedHEAD静默; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-14,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-14; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-16: 支持重复传入 `--merged` / `--no-merged` 过滤参数（#695）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #695：将 `BranchArgs.merged` 与 `no_merged` 改为 `Vec<String>` + `clap::ArgAction::Append`，并按 Git 语义（重复 `--merged` 取并集、`--no-merged` 排除已合入任一指定提交的分支）过滤分支。
**Out of scope:** `--contains`/`--no-contains`（已支持 `Vec<String>` + `Append`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `BranchArgs.merged` 与 `no_merged` 为 `Option<String>`，重复传入触发 clap 冲突报错 | `src/command/branch.rs:463-470,2459-2468` |
**Acceptance criteria:**
- [ ] `libra branch --merged tipA --merged tipB` 的名称集合恰为从 `tipA` 或 `tipB` 可达的分支 `{main, a1, tipA, b1, tipB}`（执行条件：退出码 0）。
- [ ] `libra branch --no-merged tipA --no-merged tipB` 的名称集合恰为从 `tipA`、`tipB` 均不可达的分支 `{lone}`（Git `for-each-ref` 文档：多个 `--merged` 取并集，`--no-merged` 排除任一可达者）（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；`main` 在根提交 `r`；`a1` = `r`→`x1`，`tipA` = `x1`→`x2`；`b1` = `r`→`y1`，`tipB` = `y1`→`y2`；`lone` = `r`→`z1`。
- AC1、AC2 → `command::branch_test::repeated_merged_filters`：判据名称集合相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::repeated_merged_filters)'`（卡特有用例，new；覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-15, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=重复--merged/--no-merged过滤; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-15,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-15; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-17: 支持 `libra branch --forked <pattern>` 分叉分支列表（#696）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #696（对应 Git `6de20f6092` `t/t3200-branch.sh#179`）：在 `BranchArgs` 中新增可重复的 `--forked=<pattern>`（必填分支/远程模式参数，隐含 `--list`），按分支**已配置的 upstream**（字面量与通配符）过滤本地分支，重复取并集，并支持附加位置 `<pattern>` 收窄及与 `--no-merged`、`--format` 组合；`--forked <remote>`（经 `<remote>/HEAD`）不在本卡（`DEFER-BR-07`）。
**Out of scope:** 全局 `-C` 标志（由 `DEP-BR-03` / `#686` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `BranchArgs` 未定义 `--forked` 选项 | `src/command/branch.rs:331-508` |
| Git `6de20f6092` `t/t3200-branch.sh#179`：`--forked` 参数必填可重复，按 configured upstream 过滤，未知分支/模式报错 | `gh issue view 696` |
**Acceptance criteria:**
- [ ] `libra branch --forked origin/one --format='%(refname:short)'` 的 stdout 恰为 `local-one\n`（执行条件：退出码 0）。
- [ ] `libra branch --forked local-base --forked 'other/*' --format='%(refname:short)'` 的 stdout 恰为 `local-foreign\nlocal-onbase\n`（重复 `--forked` 取并集，字面量与 glob 混用）（执行条件：退出码 0）。
- [ ] `libra branch --forked 'origin/*' --no-merged origin/one --format='%(refname:short)'` 的 stdout 恰为 `local-one\n`（`#696` Acceptance 2、4）（执行条件：退出码 0）。
- [ ] `libra branch --forked 'origin/*' 'local-*' --format='%(refname:short)'` 的 stdout 恰为 `local-one\nlocal-two\n`（位置 `<pattern>` 收窄）（执行条件：退出码 0）。
- [ ] `libra branch --forked nope` 的 stderr 含 `not a valid branch or pattern`（执行条件：以非零状态退出）（pinned Git `builtin/branch.c:716-717`）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：按 pinned Git `t/t3200-branch.sh:1760-1791` 的 `--forked: setup` 以 Libra 命令重建：远端仓库 `forked-upstream`（分支 `main`、`one`、`two` 均在 `base`）与 `forked-other`（`foreign`）；被测仓库以 `libra remote add origin`/`other` + `libra fetch` 取得追踪 ref，建 `local-base`，以 `libra branch --track` 建 `local-one`（追 `origin/one`）、`local-two`、`local-foreign`（追 `other/foreign`）、`local-onbase`（追 `local-base`），再在 `local-one`、`local-foreign` 上各提交一次；`main` 追 `origin/main`。
- AC1–AC5 → `command::branch_test::forked_branch_listing`：AC1–AC4 判据 stdout 字节等于 AC 所述（pinned Git `t/t3200-branch.sh:1793-1875` 同期望）；AC5 判据 stderr 含该片段。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::forked_branch_listing)'`（卡特有用例，new；覆盖 AC1–AC5）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-16, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--forked分叉分支列表; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-16,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-16; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-18: 支持 `libra branch --sort=objecttype` 排序键（#699）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #699：在 `sort_branch_entries` 中支持 `objecttype` 与 `-objecttype` 排序键（并列时按 refname 升序 tie-break）。
**Out of scope:** 重复 `--sort` 多键排序（由 `BR-19` 承接）；`ahead-behind` 原子与排序（由 `BR-20`、`BR-23` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `sort_branch_entries` 允许表未包含 `objecttype` | `src/command/branch.rs:3404-3416` |
**Acceptance criteria:**
- [ ] `libra branch --sort=objecttype --format='%(refname:short)'` 的 stdout 恰为 `a\nb\nc\nmain\n`（全部指向 commit，同类型按 refname 升序）（执行条件：退出码 0）。
- [ ] `libra branch --sort=-objecttype --format='%(refname:short)'` 的 stdout 恰为 `a\nb\nc\nmain\n`（pinned Git 实测反向时并列仍按 refname 升序）（执行条件：退出码 0）。
- [ ] 配置 `branch.sort=-refname` 且当前分支为 `b` 时，`libra branch --no-sort --sort=objecttype` 的 stdout 恰为 `"  a\n* b\n  c\n  main\n"`（`--no-sort` 取消配置键，并列回退 refname 升序；`#699` Acceptance 2、3，pinned Git `t/t3200-branch.sh:1639-1657`）（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；`main` 上一个提交，另按 `b`、`a`、`c` 顺序建分支。
- AC1–AC3 → `command::branch_test::sort_objecttype`：AC3 先 `libra config branch.sort -- -refname`（值以 `-` 开头须用 `--` 分隔，实测 `libra config branch.sort -refname` 以 exit 129 `unexpected argument '-r'` 失败）与 `libra switch b`；判据 stdout 字节等于 AC 所述。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::sort_objecttype)'`（卡特有用例，new；覆盖 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-17, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--sort=objecttype排序键; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-17,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-17; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-19: 支持重复 `--sort` 多键排序（#700 部分 1/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #700 中「重复 `--sort` 键被拒绝」轴：将 `BranchArgs.sort` 改为 `Vec<String>` + `ArgAction::Append`，按 Git 优先级规则（后指定的 `--sort` 为更高优先级主键，先前键依次作为次级 tie-break，最终回退 `refname`）执行多键排序；`--no-sort` 清除此前所有键。
**Out of scope:** `%(ahead-behind:<committish>)` 格式原子（由 `BR-20` 承接）；`--sort=ahead-behind:<rev>`（由 `BR-23` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `BranchArgs.sort` 为单值 `Option<String>`，重复 `--sort` 触发 clap 冲突 | `src/command/branch.rs:477-478,2552-2578` |
**Acceptance criteria:**
- [ ] `libra branch --sort=-refname --sort=-committerdate --format='%(refname:short)'` 的 stdout 恰为 `z2\ny2\nmain\nb1\na1\n`（后给出的键为主键，同日期时按先给出的 `-refname` 降序；pinned Git 实测）（执行条件：退出码 0）。
- [ ] `libra branch --sort=-refname --no-sort --sort=committerdate --format='%(refname:short)'` 的 stdout 与 `libra branch --sort=committerdate --format='%(refname:short)'` 字节相同（`--no-sort` 清除此前键）（执行条件：退出码 0）。
- [ ] `libra branch --sort=-refname --no-sort --format='%(refname:short)'` 的 stdout 与 `libra branch --format='%(refname:short)'` 字节相同（回退默认 refname 排序）（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；提交 `one` 的提交者时间 `2020-01-01T00:00:00Z`，其上建 `b1`、`a1`；提交 `two` 的提交者时间 `2021-01-01T00:00:00Z`（`main` 所在），其上建 `z2`、`y2`（提交者时间经 `run_libra_command_with_env` 注入 `GIT_COMMITTER_DATE`，Libra 读取该变量见 `src/command/commit.rs:919-920`）。
- AC1–AC3 → `command::branch_test::repeated_sort_keys`：AC1 判据 stdout 字节等于 AC 所述（只取最后一个键的错误实现会得到 `main\ny2\nz2\na1\nb1\n`，可区分）；AC2、AC3 判据两次 stdout 字节相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::repeated_sort_keys)'`（卡特有用例，new；覆盖 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-18, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=重复--sort多键排序; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-18,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-18; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-20: 支持 `%(ahead-behind:<committish>)` 格式原子（#700 部分 2/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #700 中「`ahead-behind` 格式原子不被支持」轴：在 `src/command/for_each_ref.rs` 的原子引擎中实现 `%(ahead-behind:<committish>)`（输出 `<ahead> <behind>` 计数），供 `libra branch --format` 与 `libra for-each-ref --format` 共享使用（ADR-BR-01）。
**Out of scope:** 重复 `--sort` 键（已由 `BR-19` 交付）；`--sort=ahead-behind:<rev>`（已自本卡拆出，由 `BR-23` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `for_each_ref.rs` 原子列表无 `ahead-behind:` 处理，直接返回 unsupported atom 错误 | `src/command/for_each_ref.rs:1580-1615,1646-1660` |
| `super::status::upstream_ahead_behind`（定义于 `src/command/status.rs:883`，`branch.rs:2794` 调用）已提供两提交间 ahead/behind 计数实现 | `src/command/status.rs:883`、`src/command/branch.rs:2794` |
**Acceptance criteria:**
- [ ] `libra for-each-ref --format='%(refname:short) %(ahead-behind:other)' refs/heads` 的 stdout 恰为 `main 2 1\nother 0 0\n`（执行条件：退出码 0）。
- [ ] `libra branch --format='%(refname:short) %(ahead-behind:other)'` 的 stdout 恰为 `main 2 1\nother 0 0\n`（执行条件：退出码 0）。
- [ ] `libra for-each-ref --format='%(ahead-behind:nope)' refs/heads` 的 stderr 含 `failed to find 'nope'`（执行条件：以非零状态退出）（pinned Git 实测 exit 128 + 同文案）。
- [ ] `libra for-each-ref --format='%(ahead-behind:)' refs/heads` 的 stderr 含 `expected format: %(ahead-behind:<committish>)`（执行条件：以非零状态退出）（pinned Git 实测）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；`main` = `c1`→`c2`→`c3`；`other` = `c1`→`o1`（`main` 相对 `other` 领先 2、落后 1）。
- AC1、AC3、AC4 → `command::for_each_ref_test::ahead_behind_atom`：AC1 判据 stdout 字节相等；AC3、AC4 判据 stderr 含该片段。
- AC2 → `command::branch_test::format_ahead_behind_atom`：判据 stdout 字节相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::for_each_ref_test::ahead_behind_atom)'`（卡特有用例，new；覆盖 AC1、AC3、AC4）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::format_ahead_behind_atom)'`（卡特有用例，new；覆盖 AC2）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::for_each_ref::color_spec_tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享格式化引擎 `src/command/for_each_ref.rs`
**Dependencies:** BR-19, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/for_each_ref.rs`, `src/command/branch.rs`, `tests/command/branch_test.rs`, `tests/command/for_each_ref_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/branch.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/for-each-ref.md`、`docs/commands/zh-CN/for-each-ref.md`、`docs/development/commands/branch.md`、`docs/development/commands/for-each-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点 `src/command/`，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=%(ahead-behind)格式原子; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=2; scope=M; deps=BR-19,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-19; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-21: 修复 `libra branch --format` 输出包含 detached HEAD 伪条目（#701）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #701：当处于 detached HEAD 状态运行 `libra branch --format=<fmt>` 时，在 `branch.rs` 与 `for_each_ref.rs` 协同支持 detached HEAD 伪条目渲染（`%(HEAD)` = `*`，`%(refname)` / `%(refname:short)` 按 HEAD reflog 区分 `(HEAD detached at <oid>)` 与移动后的 `(HEAD detached from <oid>)`；自 tag 名分离的 `(HEAD detached from <tag>)` 需导航命令在 reflog 保留输入名，属 `DEFER-BR-07`）。
**Out of scope:** `--show-current` detached HEAD 静默行为（已由 `BR-15` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `render_branch_output` 的 `--format` 分支未注入 `detached_head`，且 `for_each_ref::render_ref_format_lines` 在 detached HEAD 时设 `head_refname = None` | `src/command/branch.rs:2875-2899`、`src/command/for_each_ref.rs:1391-1410` |
**Acceptance criteria:**
- [ ] 仓库 A（以 `libra switch --detach <c1>` 分离且未移动）中 `libra branch --format='%(HEAD)|%(refname)|%(refname:short)'` 的首行第一字段为 `*`。
- [ ] 上一条命令首行第二字段为 `(HEAD detached at <s>)`（`<s>` 为 `c1` 的 7 位短 ID；pinned Git 实测同形）。
- [ ] 上一条命令首行第三字段为 `(HEAD detached at <s>)`。
- [ ] 仓库 B（`libra switch --detach <c1>` 分离后又提交一次，HEAD 已移动）中同一命令首行第二字段为 `(HEAD detached from <s>)`（`<s>` 为 `c1` 的 7 位短 ID；pinned Git 实测同形）。
- [ ] 仓库 B 中同一命令首行第三字段为 `(HEAD detached from <s>)`。
- [ ] 仓库 A 中同一命令各行第二字段的序列恰为 `[(HEAD detached at <s>), refs/heads/main, refs/heads/side]`（伪条目在首行，其后按 Git 顺序）。
- [ ] 附着 `main` 时同一命令各行第二字段的序列恰为 `[refs/heads/main, refs/heads/side]`（不注入伪条目）。
- [ ] 仓库 A 中 `libra branch -r --format='%(refname)'`（无远程追踪分支）的 stdout 为空（执行条件：退出码 0）（不注入伪条目）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；`main` = `c1`→`c2`，另建分支 `side` 在 `c1`；仓库 A 以 `libra switch --detach <c1>` 进入 detached HEAD 且不再移动（前置 `libra rev-parse --abbrev-ref HEAD` 输出 `HEAD`）；仓库 B 同样 `libra switch --detach <c1>` 后再提交一次。`<s>` 为 `c1` 的 7 位短 ID。自 tag 名分离的 `(HEAD detached from <tag>)` 形态需 `switch`/`checkout` 在 HEAD reflog 中保留用户输入的名字（现 `src/command/switch.rs` 只记短对象 ID），不在本卡（`DEFER-BR-07`）。
- AC1–AC8 → `command::branch_test::format_includes_detached_head`（执行条件：每条命令退出 0）：AC1–AC3 在仓库 A、AC4、AC5 在仓库 B 读取首行对应字段并判定等于所述值；AC6、AC7 判据第二字段序列相等；AC8 判据 stdout 为空。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::format_includes_detached_head)'`（卡特有用例，new；覆盖 AC1–AC8）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::for_each_ref_test::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享格式化引擎 `src/command/for_each_ref.rs`
**Dependencies:** BR-20, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/command/for_each_ref.rs`, `tests/command/branch_test.rs`, `tests/command/for_each_ref_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/branch.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/for-each-ref.md`、`docs/commands/zh-CN/for-each-ref.md`、`docs/development/commands/branch.md`、`docs/development/commands/for-each-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点 `src/command/`，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--format含detachedHEAD伪条目; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=BR-20,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-20; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-22: 支持 `libra branch -r -d`/`-D` 删除远程追踪分支（#866 部分 1/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #866 中 `branch -r -d` 部分：解除 clap `action`/`query` 组对 `-r` + `-d`/`-D` 的错误互斥（仅允许 `-r` 与 `-d`/`-D` 或列表组合），解析 `<remote>/<branch>` 并删除对应的远程追踪引用及关联 reflog（与 Git 一致不接受 `refs/remotes/<remote>/<branch>` 全名：pinned Git 实测 `git branch -r -d refs/remotes/origin/feat` 以 exit 1 + `remote-tracking branch ... not found` 失败）。
**Out of scope:** `fetch --all` 跳过 `remote.<name>.skipFetchAll`（已自本卡拆出，由 `BR-24` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `ArgGroup::new("action").conflicts_with("query")` 在 clap 层拒绝 `-r -d`，且 `delete_branch_impl` 仅查本地分支 | `src/command/branch.rs:320-330,2171-2233` |
| `Branch::delete_branch_result` 已接受 `remote: Option<&str>` 参数 | `src/internal/branch.rs:829-890` |
**Acceptance criteria:**
- [ ] `libra branch -r -d origin/feat` 的 stdout 含一行匹配 `^Deleted remote-tracking branch origin/feat \(was [0-9a-f]{7}\)\.$`（pinned Git 实测同形）（执行条件：退出码 0）。
- [ ] AC1 执行前后 SNAP 仅差 `refs/remotes/origin/feat` 的 `reference` 行与 `ref_name='refs/remotes/origin/feat'` 的 `reflog` 行（这些行被删除；同名本地分支 `feat` 的 ref、`branch.feat.*` 与 `metadata_kv` 行不变）。
- [ ] `libra branch -r -D origin/feat2` 后 `reference` 表不存在 `name='refs/remotes/origin/feat2'`、`remote='origin'` 的行（远程追踪 ref 以完整目标名存储，`src/command/fetch.rs:4438-4446`）（执行条件：退出码 0）。
- [ ] `libra branch -r -d origin/nope` 的 stderr 含 `remote-tracking branch 'origin/nope' not found`（执行条件：以非零状态退出）（pinned Git 实测 exit 1 + 同文案）。
- [ ] 上一条命令执行前后 SNAP 相等。
- [ ] 在新的同形夹具中（前置：`libra rev-parse --verify refs/remotes/origin/feat` 退出 0），`libra branch -r -d refs/remotes/origin/feat` 的 stderr 含 `remote-tracking branch 'refs/remotes/origin/feat' not found`（执行条件：以非零状态退出）（不接受全名；接受全名的错误实现会删除该 ref 并退出 0；pinned Git 实测 exit 1 + 同文案）。
- [ ] FI 使删除在移除 `refs/remotes/origin/feat` 的 `reference` 行时失败：`libra branch -r -d origin/feat` 执行前后 SNAP 相等（执行条件：以非零状态退出，且 stderr 含注入标识 `br-fi`，证明失败发生在注入点）。
- [ ] FI 使删除在移除 `ref_name='refs/remotes/origin/feat'` 的 `reflog` 行时失败：`libra branch -r -d origin/feat` 执行前后 SNAP 相等（执行条件：以非零状态退出，且 stderr 含注入标识 `br-fi`，证明失败发生在注入点）（AC7、AC8 两种注入点覆盖两种删除顺序：无论实现先删哪一类行，总有一种注入发生在另一类已删除之后，从而证明整次删除在同一事务内回滚）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0 + FR（远端含 `feat`、`feat2`，`libra fetch origin` 生成两个远程追踪 ref）；本地另建分支 `feat`，预置 `branch.feat.description=keep` 与一条 `metadata_kv` 分支元数据。前置：SNAP 中存在 `ref_name='refs/remotes/origin/feat'` 的 `reflog` 行（由 `libra fetch` 写入；不存在即前置失败并按 ER-10 回报）。AC7、AC8 各在新的同形夹具上执行。远程追踪 ref 在 `reference` 表中以 `name` = 完整 `refs/remotes/origin/<b>`、`remote='origin'` 存储（`src/command/fetch.rs:4438-4446`）。AC6 在新的同形夹具上执行。
- AC1–AC6 → `command::branch_test::delete_remote_tracking_branch`：AC1 判据存在匹配行；AC2 判据两次 SNAP 删除所列行后逐字段相等且执行后不含所列行；AC3 判据所述行不存在；AC4、AC6 判据 stderr 含该文案；AC5 判据两次 SNAP 相等。
- AC7、AC8 → `command::branch_test::delete_remote_tracking_branch_rolls_back_midway_failure`：各在新的同形夹具上分别安装 FI（AC7：`BEFORE DELETE ON reference WHEN OLD.name = 'refs/remotes/origin/feat' AND OLD.remote = 'origin'`；AC8：`BEFORE DELETE ON reflog WHEN OLD.ref_name = 'refs/remotes/origin/feat'`）后取 SNAP，运行命令，再取 SNAP；判据两次 SNAP 相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::delete_remote_tracking_branch)'`（卡特有用例，new；覆盖 AC1–AC6）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::delete_remote_tracking_branch_rolls_back_midway_failure)'`（卡特有用例，new；覆盖 AC7、AC8）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^internal::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享分支存储模块 `src/internal/branch.rs`
**Dependencies:** BR-21, DEP-BR-08, DEP-BR-11, DEP-BR-12, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/internal/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-r-d删除远程追踪分支; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-21,DEP-BR-08,DEP-BR-11,DEP-BR-12,DEP-BR-14; writeset=序列化于BR-21; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-23: 支持 `libra branch --sort=[-]ahead-behind:<rev>` 排序键（#700 部分 3/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #700 中 `t3203#33` 排序轴（自 `BR-20` 拆出）：在 `sort_branch_entries` 中支持 `ahead-behind:<committish>` 与 `-ahead-behind:<committish>` 排序键，按分支相对 `<committish>` 的 ahead/behind 计数排序（匹配 Git `ahead-behind` comparator 顺序）。
**Out of scope:** `%(ahead-behind:...)` 格式原子（已在 `BR-20` 交付）；重复 `--sort`（已在 `BR-19` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `sort_branch_entries` 允许表未识别 `ahead-behind:<rev>` 排序键 | `src/command/branch.rs:3341,3404-3416` |
| `t3203#33` 测试 `branch --format="%(refname) %(ahead-behind:HEAD)" --sort=refname --sort=ahead-behind:HEAD` | `gh issue view 700` |
**Acceptance criteria:**
- [ ] 仓库 T（detached HEAD）中 `libra branch --format='%(refname) %(ahead-behind:HEAD)' --sort=refname --sort=ahead-behind:HEAD` 的 stdout 恰为 `"(HEAD detached at <s>) 0 0\nrefs/heads/ambiguous 0 0\nrefs/heads/branch-two 0 0\nrefs/heads/branch-one 1 0\nrefs/heads/main 1 0\n"`（伪条目恒在首行，同值按先给出的 refname 升序；`#700` Acceptance 3 的 `t3203` 形态，pinned Git 实测）（执行条件：退出码 0）。
- [ ] 仓库 T 中同一格式下 `--sort=refname --sort=-ahead-behind:HEAD` 的 stdout 恰为 `"(HEAD detached at <s>) 0 0\nrefs/heads/branch-one 1 0\nrefs/heads/main 1 0\nrefs/heads/ambiguous 0 0\nrefs/heads/branch-two 0 0\n"`（降序只反转主键，同值仍按 refname 升序；pinned Git 实测）（执行条件：退出码 0）。
- [ ] 仓库 U 中 `libra branch --format='%(refname:short) %(ahead-behind:HEAD)' --sort=refname --sort=ahead-behind:HEAD` 的 stdout 恰为 `main 0 0\na 0 2\nc 1 0\nb 1 2\n`（先按 ahead 再按 behind 升序；pinned Git 实测）（执行条件：退出码 0）。
- [ ] 仓库 U 中同一格式下 `--sort=refname --sort=-ahead-behind:HEAD` 的 stdout 恰为 `b 1 2\nc 1 0\na 0 2\nmain 0 0\n`（pinned Git 实测）（执行条件：退出码 0）。
- [ ] `libra branch --sort=ahead-behind:nope` 的 stderr 含 `failed to find 'nope'`（执行条件：以非零状态退出）（pinned Git 实测 exit 128）。
- [ ] `libra branch --sort=ahead-behind` 的 stderr 含 `expected format: %(ahead-behind:<committish>)`（执行条件：以非零状态退出）（pinned Git 实测 exit 128）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 仓库 T：F0；`main` 上提交 `r`、`h`；在 `h` 建 `ambiguous`、`branch-two`；`libra switch -c branch-one` 后提交 `b1`；`libra switch main` 后提交 `m1`；最后 `libra switch --detach <h>`（前置 `libra rev-parse --abbrev-ref HEAD` 输出 `HEAD`；`<s>` 为 `h` 的 7 位短 ID）。
- 仓库 U：F0；`main` = `c1`→`c2`→`c3`（当前分支）；`a` 在 `c1`；`b` = `c1`→`b1`；`c` = `c3`→`c4`。
- AC1–AC6 → `command::branch_test::sort_ahead_behind`：AC1–AC4 判据 stdout 字节等于 AC 所述；AC5、AC6 判据 stderr 含该片段。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::sort_ahead_behind)'`（卡特有用例，new；覆盖 AC1–AC6）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-22, DEP-BR-08, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--sort=ahead-behind排序键; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-22,DEP-BR-08,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-22; release=independent; split-from=BR-20; exception=N/A`

---

### Task BR-24: 支持 `fetch --all` 尊重 `remote.<name>.skipFetchAll`（#866 部分 2/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #866 中 `t5514#9` `skipFetchAll` 轴（自 `BR-22` 拆出）：在 `src/command/fetch.rs` 执行 `fetch --all` 时读取 `remote.<name>.skipFetchAll`（bool），当其为 `true` 时跳过该远端（显式指定 `libra fetch <remote>` 仍正常拉取）。
**Out of scope:** `branch -r -d` 删除远程追踪分支（已在 `BR-22` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `src/` 全仓尚无 `skipFetchAll` 配置读取 | `rg -n skipFetchAll src/` 无命中 |
| `#866` Acceptance 3–4 要求 `remote.<name>.skipFetchAll=true` 使 `fetch --all` 跳过该远端 | `gh issue view 866` |
**Acceptance criteria:**
- [ ] 远端 `one`、`two`、`three` 均已配置且 `remote.three.skipFetchAll=true` 时，`libra fetch --all` 后 `libra rev-parse refs/remotes/one/main` 输出远端 `one` 的 `main` 提交 ID（执行条件：退出码 0）。
- [ ] 同一命令后 `libra rev-parse refs/remotes/two/main` 输出远端 `two` 的 `main` 提交 ID。
- [ ] 同一命令后 `libra rev-parse --verify refs/remotes/three/main` 以非零状态退出（`three` 被跳过；`#866` Acceptance 4）。
- [ ] 随后 `libra fetch three` 后 `libra rev-parse refs/remotes/three/main` 输出远端 `three` 的 `main` 提交 ID（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具：F0；三个本地路径仓库 `one`、`two`、`three` 各有提交互不相同的 `main`；被测仓库 `libra remote add` 三者，`libra config remote.three.skipFetchAll true`。
- AC1–AC4 → `command::fetch_test::skip_fetch_all`：AC1、AC2、AC4 判据输出等于对应远端 `main` 的提交 ID；AC3 判据 `rev-parse --verify` 的退出码 ≠ 0。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::fetch_test::skip_fetch_all)'`（卡特有用例，new；覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::fetch::tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-23, DEP-BR-08, DEP-BR-10, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` 与 `COMPATIBILITY.md` 中 `remote.<name>.skipFetchAll` 说明。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=fetch--all尊重skipFetchAll; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-23,DEP-BR-08,DEP-BR-10,DEP-BR-14; writeset=序列化于BR-23; release=independent; split-from=BR-22; exception=N/A`

---

### Task BR-40: 交付 `@{push}` 过渡 fail-closed 守卫（#684 部分 4/9 前置）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 自 `BR-25` 拆出（G-09）：先在 `src/utils/util.rs` 交付并接入最小的 `@{push}` 修订解析入口，使 CLI 能识别该语法并读取相关配置；随后对 7 个尚未实现的配置分支返回可区分的过渡拒绝，避免把“尚不支持 `@{push}`”误当作各项配置的拒绝证据。`BR-25` 在同一入口上扩展 upstream 与 remote 选择。
**Out of scope:** upstream 与 remote 选择（`BR-25`）；`simple`/`nothing`/`matching`/`current`/自定义 refspec/`mirror` 的完整实现（`BR-27`/`BR-26`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| 原 `BR-25` 同时承载 7 项过渡守卫、upstream 与 remote 选择，造成 G-03 计数和发布边界争议 | `src/utils/util.rs:2649`；原 `BR-25`/`EX-BR-01` |
**Acceptance criteria:**
- [ ] 仅配置 `remote.origin.push=refs/heads/main:refs/heads/published`（`push.default` 未配置）时，`libra rev-parse @{push}` 的 stderr 含 `@{push} is not yet supported for remote.origin.push`（按 Git `remote.c:1977-1990` 先查 push refspec）（执行条件：以非零状态退出）。
- [ ] 仅配置 `remote.origin.mirror=true` 时，`libra rev-parse @{push}` 的 stderr 含 `@{push} is not yet supported for remote.origin.mirror`（Git `remote.c:1992-1993`）（执行条件：以非零状态退出）。
- [ ] 未配置 `push.default`（默认 `simple`）时，`libra rev-parse @{push}` 的 stderr 含 `@{push} is not yet supported for push.default=simple`（执行条件：以非零状态退出）。
- [ ] `push.default=simple` 时，`libra rev-parse @{push}` 的 stderr 含 `@{push} is not yet supported for push.default=simple`（执行条件：以非零状态退出）。
- [ ] `push.default=nothing` 时，`libra rev-parse @{push}` 的 stderr 含 `@{push} is not yet supported for push.default=nothing`（执行条件：以非零状态退出）。
- [ ] `push.default=matching` 时，`libra rev-parse @{push}` 的 stderr 含 `@{push} is not yet supported for push.default=matching`（执行条件：以非零状态退出）。
- [ ] `push.default=current` 时，`libra rev-parse @{push}` 的 stderr 含 `@{push} is not yet supported for push.default=current`（执行条件：以非零状态退出）。
- [ ] `push.default=upstream` 时，`libra rev-parse @{push}` 的 stderr 含 `@{push} is not yet supported for push.default=upstream`（`BR-25` 解除）（执行条件：以非零状态退出）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具 FP：F0 + FR（远端 `origin` 含 `main`、`topic`、`feature`、`published`；另一远端 `other` 含 `main`，均 `libra fetch` 生成追踪 ref，远端各分支提交互不相同且与本地 `main` 不同）；本地 `main` 追 `origin/main`（`branch.main.remote=origin`、`branch.main.merge=refs/heads/main`），`remote.origin.fetch`/`remote.other.fetch` 为默认 `+refs/heads/*:refs/remotes/<r>/*`。
- AC1–AC8 → `command::rev_parse_test::push_transition_fail_closed_guards`：每条在只含该 AC 所述配置的新夹具上运行；判据 stderr 含 AC 所列整句（整句模板 `@{push} is not yet supported for <K>`，`<K>` 为被选 push remote 的 `remote.<r>.push` / `remote.<r>.mirror`，或 `push.default=<值>`，未配置时取 `push.default=simple`）（各句以配置键或取值区分，单一笼统错误无法同时满足不同 AC 的整句）。后续解除守卫的卡（`BR-25` 解除 AC8、`BR-27` 解除 AC3/AC4、`BR-26` 解除 AC1/AC2/AC5–AC7）同卡删除本用例中对应分支并以共享回归组运行本用例。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::push_transition_fail_closed_guards)'`（卡特有用例，new；覆盖 AC1–AC8）
**Full-suite trigger:** T-1: 修改共享修订解析 `src/utils/util.rs`
**Dependencies:** BR-24, DEP-BR-08, DEP-BR-09, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 与 `COMPATIBILITY.md`（登记 7 项未实现配置及 `push.default=upstream` 的过渡 fail-closed 边界与后续清除责任）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{push}过渡fail-closed守卫; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-24,DEP-BR-08,DEP-BR-09,DEP-BR-14; writeset=序列化于BR-24; release=independent; split-from=BR-25; exception=N/A`

---

### Task BR-25: 支持 `@{push}` remote 选择与 `upstream`（#684 部分 4/9）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #684 中 `@{push}` 基础解析与 remote 选择轴（自 `BR-06` 拆出）：在 `BR-40` 已交付的最小 `@{push}` 入口上扩展当前分支的 `pushRemote` → `pushDefault` → `branch.remote` → 唯一远端/`origin` 选择（Git `remote.c:672-711`）与 `push.default=upstream`，而不是重新新增一套解析器。7 项过渡 fail-closed 守卫已前置到 `BR-40`，本卡不再合并该门族。
**Out of scope:** `@{upstream}`/`@{u}`（已在 `BR-06`/`BR-32`/`BR-31` 交付）；过渡守卫（`BR-40`）；`<branch>@{push}`（`BR-29`）；`simple` 与 `tracking_for_push_dest`（`BR-27`）；detached HEAD（`BR-30`）；`nothing`/`matching`/`current`/refspec/`mirror`（`BR-26`）；`%(push)`（`BR-28`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `util.rs:2649` 尚未支持 `@{push}` 修订解析 | `src/utils/util.rs:2649`、`src/command/for_each_ref.rs:250-262` |
**Acceptance criteria:**
- [ ] `push.default=upstream` 时，在 `main` 上 `libra rev-parse @{push}` 的 stdout 为 `refs/remotes/origin/main` 的提交 ID（执行条件：退出码 0）。
- [ ] 另配 `branch.main.pushRemote=other` 时，`push.default=upstream` 下 `libra rev-parse @{push}` 的 stdout 仍为 `refs/remotes/origin/main` 的提交 ID（Git `remote.c:2003-2004` 对 `upstream` 直接返回 upstream、不校验 push remote；pinned Git 实测）（执行条件：退出码 0）。
- [ ] `remote.other.push` 已配置时，同时设 `branch.main.pushRemote=other` 与 `remote.pushDefault=origin`，`libra rev-parse @{push}` 的 stderr 含 `@{push} is not yet supported for remote.other.push`（执行条件：以非零状态退出）（`pushRemote` 优先于 `pushDefault`，Git `remote.c:701-711`）。
- [ ] `remote.other.push` 已配置时，同时设 `branch.main.pushRemote=origin` 与 `remote.pushDefault=other`，`libra rev-parse @{push}` 的 stdout 为 `refs/remotes/origin/main` 的提交 ID（执行条件：退出码 0）。
- [ ] `remote.other.push` 已配置时，只设 `remote.pushDefault=other`（`branch.main.remote=origin`），`libra rev-parse @{push}` 的 stderr 含 `@{push} is not yet supported for remote.other.push`（执行条件：以非零状态退出）（`pushDefault` 优先于 `branch.<name>.remote`）。
- [ ] 只配置唯一远端 `other`（含 `remote.other.push`）且 `main` 无 `branch.main.remote` 时，`libra rev-parse @{push}` 的 stderr 含 `@{push} is not yet supported for remote.other.push`（执行条件：以非零状态退出）（唯一远端回退，Git `remote.c:683-684`）。
- [ ] 同时配置 `origin`（含 `remote.origin.push`）与 `other` 且 `main` 无 `branch.main.remote` 时，`libra rev-parse @{push}` 的 stderr 含 `@{push} is not yet supported for remote.origin.push`（执行条件：以非零状态退出）（多远端回退 `origin`，Git `remote.c:685`）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具 FP：F0 + FR（远端 `origin` 含 `main`、`topic`、`feature`、`published`；另一远端 `other` 含 `main`，均 `libra fetch` 生成追踪 ref，远端各分支提交互不相同且与本地 `main` 不同）；本地 `main` 追 `origin/main`（`branch.main.remote=origin`、`branch.main.merge=refs/heads/main`），`remote.origin.fetch`/`remote.other.fetch` 为默认 `+refs/heads/*:refs/remotes/<r>/*`。各条另配 `push.default=upstream`；`remote.<r>.push` 取 `refs/heads/main:refs/heads/main`；AC6 的夹具不添加 `origin`。
- AC1–AC7 → `command::rev_parse_test::push_remote_and_upstream`：每条在只含该 AC 所述配置的新夹具上运行；AC1、AC2、AC4 判据 stdout 等于 `libra rev-parse refs/remotes/origin/main` 的输出；AC3、AC5–AC7 判据 stderr 含所列整句（依赖 `BR-40` 的 `remote.<r>.push` 过渡拒绝观察被选中的 remote；`BR-26` 解除该守卫时同卡把这些预期改为对应 refspec 映射结果）。
- 同卡删除 `command::rev_parse_test::push_transition_fail_closed_guards` 中 `push.default=upstream` 分支（`BR-40` AC8）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::push_remote_and_upstream)'`（卡特有用例，new；覆盖 AC1–AC7）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::push_transition_fail_closed_guards)'`（共享回归组，不计入 G-03 VER；本卡已删除其 `upstream` 分支）
**Full-suite trigger:** T-1: 修改共享修订解析 `src/utils/util.rs`
**Dependencies:** BR-40, DEP-BR-08, DEP-BR-09, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 与 `COMPATIBILITY.md`（注明 `upstream` 模式下 `@{push}` 直接返回 upstream、不校验 push remote 与 upstream remote 一致（Git `remote.c:2003-2004`）及 remote 选择优先级，移除 `BR-40` 对 `push.default=upstream` 的过渡拒绝，其他过渡拒绝由后续卡按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，公开修订语法）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{push}remote选择与upstream; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-40,DEP-BR-08,DEP-BR-09,DEP-BR-14; writeset=序列化于BR-40; release=independent; split-from=BR-06; exception=N/A`

---

### Task BR-29: 支持 `<branch>@{push}`、大小写变体与分支名解释（#684 部分 5/9）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #684 中 `@{push}` 显式分支、大小写变体与 `branch` 分支名解释轴（自 `BR-25` 拆出；注：`BR-27` 合入前本卡测试夹具显式配置 `push.default=upstream`）：在 `src/utils/util.rs` 补齐 `<branch>@{push}` 与 `@{PUSH}`/`@{pUSh}`/`@{PuSH}` 大小写解析，并在 `src/command/branch.rs` 接入分支名解释。
**Out of scope:** remote 选择与 `upstream`（已在 `BR-25` 交付）；`simple` 与 `tracking_for_push_dest`（拆出 `BR-27`）；detached HEAD（拆出 `BR-30`）；`nothing`/`matching`/`current`/refspec/`mirror`（拆出 `BR-26`）；`%(push)` 对齐（拆出 `BR-28`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `util.rs` 与 `branch.rs` 尚未接入 `<branch>@{push}`、大小写变体与 `branch` 分支名解释 | `src/utils/util.rs:2649`、`src/command/branch.rs` |
**Acceptance criteria:**
- [ ] `push.default=upstream`、`topic` 追 `origin/topic` 时，在 `main` 上 `libra rev-parse topic@{push}` 的 stdout 为 `refs/remotes/origin/topic` 的提交 ID（夹具中各远程追踪 ref 提交互不相同，故提交相等即指向该 ref）（执行条件：退出码 0）。
- [ ] `libra rev-parse @{PUSH}` 的 stdout 与 `libra rev-parse @{push}` 相同（后缀大小写不敏感）（执行条件：退出码 0）。
- [ ] `libra rev-parse @{pUSh}` 的 stdout 与 `libra rev-parse @{push}` 相同（执行条件：退出码 0）。
- [ ] `libra rev-parse @{PuSH}` 的 stdout 与 `libra rev-parse @{push}` 相同（执行条件：退出码 0）。
- [ ] `libra branch -f scratch @{push}` 后 `libra rev-parse scratch` 输出 `refs/remotes/origin/main` 的提交 ID（执行条件：退出码 0）。
- [ ] 在 `main` 上 `libra branch --set-upstream-to=topic@{push}` 后 `branch.main.merge` 的值为 `refs/heads/topic`（执行条件：退出码 0）。
- [ ] `main` 的上游为本地分支 `up`（`branch.main.remote=.`、`branch.main.merge=refs/heads/up`）时，`libra branch -f @{push} two` 后 `libra rev-parse up` 输出 `two` 的提交 ID（`@{push}` 按分支名解释为 `up`）（执行条件：退出码 0）。
- [ ] `@{push}` 解析为远程追踪 ref `refs/remotes/origin/main` 时，`libra branch -f @{push} two` 执行前后 SNAP 相等（执行条件：以非零状态退出）（拒绝把远程追踪 ref 当本地分支；pinned Git 同类输入以 `is not a valid branch name` 退出 128）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具 FP：F0 + FR（远端 `origin` 含 `main`、`topic`、`feature`、`published`；另一远端 `other` 含 `main`，均 `libra fetch` 生成追踪 ref，远端各分支提交互不相同且与本地 `main` 不同）；本地 `main` 追 `origin/main`（`branch.main.remote=origin`、`branch.main.merge=refs/heads/main`），`remote.origin.fetch`/`remote.other.fetch` 为默认 `+refs/heads/*:refs/remotes/<r>/*`。各条另配 `push.default=upstream`（`BR-27` 前的过渡），`topic` 追 `origin/topic`；`scratch`、`up` 在本地根提交，`two` 在另一本地提交。
- AC1–AC4 → `command::rev_parse_test::push_explicit_branch_and_case`：AC1 判据 stdout 等于 `libra rev-parse refs/remotes/origin/topic` 的输出；AC2–AC4 判据与 `@{push}` 的 stdout 相等。
- AC5–AC8 → `command::branch_test::branch_push_name_interpretation`：AC5 判据 `scratch` 提交相符；AC6 判据配置值相符；AC7 在改配本地上游的新夹具上运行，判据 `up` 提交相符；AC8 判据命令前后 SNAP 相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::push_explicit_branch_and_case)'`（卡特有用例，new；覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::branch_push_name_interpretation)'`（卡特有用例，new；覆盖 AC5–AC8）
**Full-suite trigger:** T-1: 修改共享修订解析 `src/utils/util.rs`
**Dependencies:** BR-25, DEP-BR-08, DEP-BR-09, DEP-BR-11, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `src/command/branch.rs`, `tests/command/rev_parse_test.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/branch.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件，公开修订与分支名解释语法）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{push}显式分支与大小写及分支名解释; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-25,DEP-BR-08,DEP-BR-09,DEP-BR-11,DEP-BR-14; writeset=序列化于BR-25; release=independent; split-from=BR-25; exception=N/A`

---

### Task BR-27: 支持 `@{push}` `simple` 校验与 `tracking_for_push_dest`（#684 部分 6/9）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #684 中 `push.default=simple`（含未显式配置时的默认）与 `tracking_for_push_dest` fetch refspec 追踪映射轴（自 `BR-25` 拆出）：对齐 Git `remote.c:1963-2025`（`branch_get_push_1`），先检查 upstream 与 `tracking_for_push_dest(remote, branch)`（缺失分别报 upstream 缺失或 `has no local tracking branch`），仅当两者均存在且相等时成功，两者均存在但不同时报错 `cannot resolve 'simple' push to a single destination`。
**Out of scope:** remote 选择与 `upstream`（已在 `BR-25` 交付）；detached HEAD 规则（拆出 `BR-30`）；`nothing`/`matching`/`current`/自定义 refspec/`mirror`（拆出 `BR-26`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `util.rs` 尚未实现 `simple` 与 `tracking_for_push_dest` 追踪映射 | `src/utils/util.rs:2649` |
**Acceptance criteria:**
- [ ] 未配置 `push.default` 时，在 `main` 上 `libra rev-parse @{push}` 的 stdout 为 `refs/remotes/origin/main` 的提交 ID（upstream 与 `tracking_for_push_dest` 相同）。
- [ ] `push.default=simple` 时同一命令的 stdout 为 `refs/remotes/origin/main` 的提交 ID。
- [ ] `remote.origin.fetch=+refs/heads/*:refs/remotes/o2/*` 且 `refs/remotes/o2/main` 已由 `libra fetch origin` 生成（`refs/remotes/origin/main` 不存在）时，`libra rev-parse @{push}` 的 stdout 为 `refs/remotes/o2/main` 的提交 ID（push 目标经 fetch refspec 映射）（执行条件：退出码 0）。
- [ ] `main` 无上游配置时 `libra rev-parse @{push}` 的 stderr 含 `no upstream configured for branch 'main'`（先于 `simple` 比较，Git `remote.c:2012-2014`）。
- [ ] 分支 `feature` 追 `origin/main` 且 `remote.origin.fetch` 只含 `+refs/heads/main:refs/remotes/origin/main` 时，`libra rev-parse feature@{push}` 的 stderr 含 `push destination 'refs/heads/feature' on remote 'origin' has no local tracking branch`（Git `remote.c:1956-1959`）。
- [ ] 分支 `feature` 追 `origin/main`（默认 fetch refspec）时，`libra rev-parse feature@{push}` 的 stderr 含 `cannot resolve 'simple' push to a single destination`（同远端不同名；pinned Git 实测）。
- [ ] `branch.main.pushRemote=other` 时，`libra rev-parse @{push}` 的 stderr 含 `cannot resolve 'simple' push to a single destination`（三角配置；pinned Git 实测）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具 FP：F0 + FR（远端 `origin` 含 `main`、`topic`、`feature`、`published`；另一远端 `other` 含 `main`，均 `libra fetch` 生成追踪 ref，远端各分支提交互不相同且与本地 `main` 不同）；本地 `main` 追 `origin/main`（`branch.main.remote=origin`、`branch.main.merge=refs/heads/main`），`remote.origin.fetch`/`remote.other.fetch` 为默认 `+refs/heads/*:refs/remotes/<r>/*`。
- AC1–AC7 → `command::rev_parse_test::push_simple_and_tracking`：每条在只含该 AC 所述配置的新夹具上运行；AC1–AC3 判据 stdout 等于所述值；AC4–AC7 判据 stderr 含所列文案。
- 同卡删除 `command::rev_parse_test::push_transition_fail_closed_guards` 中未配置 `push.default` 与 `push.default=simple` 两个分支（`BR-40` AC3、AC4）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::push_simple_and_tracking)'`（卡特有用例，new；覆盖 AC1–AC7）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::push_transition_fail_closed_guards)'`（共享回归组，不计入 G-03 VER；本卡已删除其 `simple` 分支）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^utils::util::test::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享修订解析 `src/utils/util.rs`
**Dependencies:** BR-29, DEP-BR-08, DEP-BR-09, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 与 `COMPATIBILITY.md`（`branch --set-upstream-to=@{push}` 使用本卡扩展后的共享解析器，同卡更新 `branch` 行为说明）（同步清除 `BR-40` 遗留的未配置 `push.default` 与 `push.default=simple` fail-closed 过渡说明，按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，公开 `simple` 修订契约）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{push}simple与tracking_for_push_dest; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-29,DEP-BR-08,DEP-BR-09,DEP-BR-14; writeset=序列化于BR-29; release=independent; split-from=BR-25; exception=N/A`

---

### Task BR-30: 完善 `@{push}` detached HEAD 规则（#684 部分 7/9）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #684 中 `@{push}` detached HEAD 规则轴（自 `BR-27` 拆出）：区分从 `HEAD` 推断分支的隐式 `@{push}`（detached HEAD 下 fail-closed 报错）与显式 `<branch>@{push}`（detached HEAD 下按 `<branch>` 配置正常解析）。
**Out of scope:** `simple` 与 `tracking_for_push_dest`（已在 `BR-27` 交付）；`nothing`/`matching`/`current`/自定义 refspec/`mirror`（拆出 `BR-26`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `util.rs` 尚未实现 detached HEAD 下隐式 `@{push}` 报错与显式 `<branch>@{push}` 区分 | `src/utils/util.rs:2649` |
**Acceptance criteria:**
- [ ] detached HEAD 下 `libra rev-parse @{push}` 的 stderr 含 `HEAD does not point to a branch`（执行条件：以非零状态退出）（Git `remote.c:2035-2036`）。
- [ ] detached HEAD 下 `libra rev-parse main@{push}` 的 stdout 为 `refs/remotes/origin/main` 的提交 ID（未配置 `push.default`）（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具 FP：F0 + FR（远端 `origin` 含 `main`、`topic`、`feature`、`published`；另一远端 `other` 含 `main`，均 `libra fetch` 生成追踪 ref，远端各分支提交互不相同且与本地 `main` 不同）；本地 `main` 追 `origin/main`（`branch.main.remote=origin`、`branch.main.merge=refs/heads/main`），`remote.origin.fetch`/`remote.other.fetch` 为默认 `+refs/heads/*:refs/remotes/<r>/*`。进入 detached HEAD 后前置 `libra rev-parse --abbrev-ref HEAD` 输出 `HEAD`。
- AC1、AC2 → `command::rev_parse_test::push_detached_head`：AC1 判据 stderr 含该文案；AC2 判据 stdout 等于所述值。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::push_detached_head)'`（卡特有用例，new；覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^utils::util::test::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享修订解析 `src/utils/util.rs`
**Dependencies:** BR-27, DEP-BR-08, DEP-BR-09, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 与 `COMPATIBILITY.md`（`branch --set-upstream-to=@{push}` 使用本卡扩展后的共享解析器，同卡更新 `branch` 行为说明）（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，公开 detached HEAD `@{push}` 契约）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{push}detachedHEAD规则; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-27,DEP-BR-08,DEP-BR-09,DEP-BR-14; writeset=序列化于BR-27; release=independent; split-from=BR-27; exception=N/A`

---

### Task BR-26: 补齐 `@{push}` `nothing`/`matching`/`current`/refspec/`mirror`（#684 部分 8/9）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #684 中其余推送模式与自定义 refspec/`mirror` 轴（自 `BR-25` 拆出）：完善 `util.rs` 共享解析器的 `nothing`/`matching`/`current`、`remote.<r>.push` 自定义 refspec 与 `mirror`（复用 `BR-27` 已交付的 `tracking_for_push_dest` 追踪映射），并同步清除 `revision`/`rev-parse` 文档中关于 refspec/mirror 的过渡 fail-closed 说明。
**Out of scope:** `tracking_for_push_dest`（已在 `BR-27` 交付）；`for-each-ref --format='%(push)'` 迁移复用（拆出 `BR-28`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `util.rs` 尚未支持 `nothing`/`matching`/`current`、自定义 refspec 与 `mirror` | `src/utils/util.rs:2649` |
**Acceptance criteria:**
- [ ] `push.default=nothing` 时 `libra rev-parse @{push}` 的 stderr 含 `push has no destination (push.default is 'nothing')`（pinned Git 实测）。
- [ ] `push.default=current` 时，在 `feature`（追 `origin/main`）上 `libra rev-parse @{push}` 的 stdout 为 `refs/remotes/origin/feature` 的提交 ID。
- [ ] `push.default=matching` 时同一命令的 stdout 为 `refs/remotes/origin/feature` 的提交 ID（Git `remote.c:1998-2001` 与 `current` 同映射）。
- [ ] `remote.origin.push=refs/heads/main:refs/heads/published` 时，在 `main` 上 `libra rev-parse @{push}` 的 stdout 为 `refs/remotes/origin/published` 的提交 ID。
- [ ] `remote.origin.push=refs/heads/other:refs/heads/x` 时，在 `main` 上 `libra rev-parse @{push}` 的 stderr 含 `push refspecs for 'origin' do not include 'main'`（pinned Git 实测）。
- [ ] `remote.origin.mirror=true` 且 `main` 无上游配置（未设 `branch.main.remote`/`merge`）时，在 `main` 上 `libra rev-parse @{push}` 的 stdout 为 `refs/remotes/origin/main` 的提交 ID（mirror 直接经 `tracking_for_push_dest` 映射，不要求 upstream，Git `remote.c:1992-1993`；忽略 mirror 而走默认 `simple` 的实现会以 upstream 缺失失败）（执行条件：退出码 0）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具 FP：F0 + FR（远端 `origin` 含 `main`、`topic`、`feature`、`published`；另一远端 `other` 含 `main`，均 `libra fetch` 生成追踪 ref，远端各分支提交互不相同且与本地 `main` 不同）；本地 `main` 追 `origin/main`（`branch.main.remote=origin`、`branch.main.merge=refs/heads/main`），`remote.origin.fetch`/`remote.other.fetch` 为默认 `+refs/heads/*:refs/remotes/<r>/*`。`feature` 的上游为 `origin/main`。
- AC1–AC6 → `command::rev_parse_test::push_modes_and_refspec`：每条在只含该 AC 所述配置的新夹具上运行；成功类判据 stdout 等于所述值，失败类判据 stderr 含所列文案。
- 同卡删除 `command::rev_parse_test::push_transition_fail_closed_guards` 中其余分支（`BR-40` AC1、AC2、AC5–AC7；删除后该用例若无剩余分支则整体删除），并把 `command::rev_parse_test::push_remote_and_upstream` 中依赖 `remote.<r>.push` 过渡拒绝的预期（`BR-25` AC3、AC5–AC7）改为对应 refspec 映射后的 `@{push}`（`refs/remotes/<被选 remote>/main` 的提交 ID）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::push_modes_and_refspec)'`（卡特有用例，new；覆盖 AC1–AC6）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::push_remote_and_upstream)'`（共享回归组，不计入 G-03 VER；本卡已按映射结果更新其预期）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^utils::util::test::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享修订解析 `src/utils/util.rs`
**Dependencies:** BR-30, DEP-BR-08, DEP-BR-09, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 与 `COMPATIBILITY.md`（`branch --set-upstream-to=@{push}` 使用本卡扩展后的共享解析器，同卡更新 `branch` 行为说明）（同步清除 `BR-40` 遗留的全部 `nothing`/`matching`/`current`/refspec/`mirror` fail-closed 过渡说明，按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，公开修订语法与模式支持）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=nothing/matching/current/refspec/mirror; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-30,DEP-BR-08,DEP-BR-09,DEP-BR-14; writeset=序列化于BR-30; release=independent; split-from=BR-25; exception=N/A`

---

### Task BR-28: 将 `for-each-ref --format='%(push)'` 迁移复用 `util.rs` 共享解析器（#684 部分 9/9）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #684 中 `for-each-ref` 单一事实源收敛轴（自 `BR-26` 拆出）：将 `src/command/for_each_ref.rs:250-262` 的私有 `resolve_pushes` 迁移至复用 `src/utils/util.rs` 共享 `@{push}` 解析器（消除 `utils` 反向依赖 `command`），对齐 Git `%(push)` 输出并透传配置读取错误。
**Out of scope:** `@{push}` 规则本体（已在 `BR-25`/`BR-29`/`BR-27`/`BR-30`/`BR-26` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `for_each_ref.rs:250-262` 仍持有私有 `resolve_pushes`，对三角失败等场景输出与 `util.rs` 不一致 | `src/command/for_each_ref.rs:250-262`、`src/utils/util.rs:2649` |
**Acceptance criteria:**
- [ ] `remote.origin.push=refs/heads/main:refs/heads/published` 时，`libra for-each-ref --format='%(refname:short) %(push)' refs/heads/main` 的 stdout 恰为 `main refs/remotes/origin/published\n`（经 `util.rs` 共享解析器；pinned Git 实测同输出）（执行条件：退出码 0）。
- [ ] `branch.main.pushRemote=other` 且未配置 `push.default`（`simple` 三角失败）时，同一命令的 stdout 恰为 `main \n`（`%(push)` 为空串，不输出 `refs/remotes/...`；Git `ref-filter.c` 对 `branch_get_push` 失败输出空值）（执行条件：退出码 0）。
- [ ] `push.default=bogus` 时，`libra for-each-ref --format='%(push)' refs/heads/main` 的 stderr 含 `push.default`（执行条件：以非零状态退出）（配置读取错误被透传而非静默为空；pinned Git 实测 exit 128 + `malformed value for push.default: bogus`）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F0/FR/SNAP/FI/NOESC 见「验证执行协议」第 6 条；每条只写判定该 AC 所需的检查）
- 夹具 FP：F0 + FR（远端 `origin` 含 `main`、`topic`、`feature`、`published`；另一远端 `other` 含 `main`，均 `libra fetch` 生成追踪 ref，远端各分支提交互不相同且与本地 `main` 不同）；本地 `main` 追 `origin/main`（`branch.main.remote=origin`、`branch.main.merge=refs/heads/main`），`remote.origin.fetch`/`remote.other.fetch` 为默认 `+refs/heads/*:refs/remotes/<r>/*`。
- AC1–AC3 → `command::for_each_ref_test::push_atom_custom_refspec_and_errors`：每条在只含该 AC 所述配置的新夹具上运行；AC1、AC2 判据 stdout 字节等于所述值；AC3 判据 stderr 含 `push.default`。
- 同卡修订既有用例 `command::for_each_ref_test::test_for_each_ref_push_atom`（`tests/command/for_each_ref_test.rs:623-713`）：其夹具只设 `branch.<name>.remote`/`merge` 而未设 `remote.<r>.fetch`（`:649-662`），按 Git 语义 upstream 无法映射时 `%(push)` 为空。本卡为夹具中每个远端补默认 `remote.<r>.fetch=+refs/heads/*:refs/remotes/<r>/*` 并生成相应远程追踪 ref；默认 `simple` 下的三角场景（`fork`/`pdef`/`lower`/`camel2` 等）预期改为空串（多个三角场景可同为空）；优先级与 remote 名大小写场景改在 `push.default=current` 下验证，使各被选 remote 的 `refs/remotes/<r>/main` 互不相同、输出可区分；每条预期以 pinned Git 对同形夹具的实测输出为准。该用例作为共享回归组运行。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（不计入卡特有 AC 分子）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::for_each_ref_test::push_atom_custom_refspec_and_errors)'`（卡特有用例，new；覆盖 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::for_each_ref_test::test_for_each_ref_push_atom)'`（共享回归组，不计入 G-03 VER；本卡已按 Git 语义更新其三角预期）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::for_each_ref::color_spec_tests::/)'`（共享回归组，不计入 G-03 VER）
**Full-suite trigger:** T-1: 修改共享格式化引擎 `src/command/for_each_ref.rs` 与 `src/utils/util.rs`
**Dependencies:** BR-26, DEP-BR-08, DEP-BR-09, DEP-BR-14
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `src/command/for_each_ref.rs`, `tests/command/for_each_ref_test.rs`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步生成器/构建派生产物，不手改、不暂存、不提交）
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/for-each-ref.md`、`docs/commands/zh-CN/for-each-ref.md`、`docs/development/commands/for-each-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件，公开格式原子对齐）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=for-each-ref%(push)复用与错误透传; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=BR-26,DEP-BR-08,DEP-BR-09,DEP-BR-14; writeset=序列化于BR-26; release=independent; split-from=BR-26; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `BranchArgs` 校验、`sort_branch_entries`、`resolve_upstream_spec` 等既有单元用例 | `source .env.test && cargo nextest run --lib -E 'test(/^command::branch::tests::/)'`（共享回归组口径） |
| 集成 | 全部 21 个 Issue 的 CLI 行为、退出码、stderr/stdout、D/F 碰撞与 `skipFetchAll` | `source .env.test && cargo nextest run --test command_test -E 'test(/^command::branch_test::/) or test(/^command::rev_parse_test::/) or test(/^command::for_each_ref_test::/) or test(/^command::fetch_test::/)'`（共享回归组口径） |
| 兼容 | 版本面同步与 `git branch`/`fetch` 对照语义 | `source .env.test && cargo nextest run --test compat_version_surface_sync` |
| 迁移 | N/A：无 SQLite schema 迁移 | N/A |
| 安全 | 跨 worktree 分支保护与不误删本地元数据仍由集成行的 `branch_test` 覆盖。D/F 两个方向分别是：`BR-10` `ref_namespace_df_conflict_create`、`BR-34` `ref_namespace_df_conflict_rename`、`BR-36` `ref_namespace_df_conflict_copy` | `source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::ref_namespace_df_conflict_create)' && source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::ref_namespace_df_conflict_rename)' && source .env.test && cargo nextest run --test command_test -E 'test(=command::branch_test::ref_namespace_df_conflict_copy)'` |
| 性能 | 继承 GC-10，列表/排序无无界扫描 | N/A |
| live/gated | 计划收口双环境全量门 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| BR-01 | Issue #678 / `branch.rs:491-508` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::column_verbose_conflict`、共享回归组 `command::branch::tests::`|
| BR-02 | Issue #679 / `branch.rs:320-365` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::list_pattern`、共享回归组 `command::branch::tests::`|
| BR-03 | Issue #680 / `branch.rs:2905-2967`; `for_each_ref.rs:2143` | `src/command/branch.rs`、`src/command/for_each_ref.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::color_branch_default_slots_match_git`、`command::branch_test::color_branch_custom_slots`、`command::branch_test::color_branch_verbose_all_layout`、`command::branch_test::color_branch_never_and_pipe_no_ansi`、共享回归组 `command::for_each_ref::color_spec_tests::`、共享回归组 `command::branch::tests::`|
| BR-33 | 已撤回 | 无写集 | 无 |N/A（本卡无独立测试命令）|
| BR-37 | 已撤回（颜色家族 `REL-BR-03` 撤回，见 `ADR-BR-02`、`DEFER-BR-03`） | 无写集 | 无 |N/A（本卡无独立测试命令）|
| BR-38 | 已撤回（颜色家族 `REL-BR-03` 撤回，见 `ADR-BR-02`、`DEFER-BR-03`） | 无写集 | 无 |N/A（本卡无独立测试命令）|
| BR-39 | 已撤回（颜色家族 `REL-BR-03` 撤回，见 `ADR-BR-02`、`DEFER-BR-03`） | 无写集 | 无 |N/A（本卡无独立测试命令）|
| BR-04 | Issue #681 / `branch.rs:1400-1432` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::set_upstream_refs_heads`、共享回归组 `command::branch::tests::`|
| BR-05 | Issue #682 / `branch.rs:1675-1687` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`、`docs/error-codes.md`|`command::branch_test::unset_upstream_without_config`、`command::branch_test::unset_upstream_with_config_succeeds`、共享回归组 `command::branch::tests::`|
| BR-06 | Issue #684 / `util.rs:2635-2675` | `src/utils/util.rs`、`src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`|`command::rev_parse_test::upstream_revision`、`command::branch_test::branch_upstream_name_interpretation`|
| BR-32 | Issue #684 / `util.rs:2635-2675` | `src/utils/util.rs`、`src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`|`command::rev_parse_test::upstream_short_and_case_variants`、`command::branch_test::branch_upstream_short_and_case_name`|
| BR-31 | Issue #684 / `util.rs:2635-2675` | `src/utils/util.rs`、`src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`|`command::rev_parse_test::upstream_detached_head`、`command::branch_test::branch_upstream_detached_explicit`|
| BR-07 | Issue #685 / `util.rs:2649-2675` | `src/utils/util.rs`、`src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`|`command::rev_parse_test::previous_branch_nth_revision`、`command::branch_test::branch_previous_branch_upstream`|
| BR-08 | Issue #687 / `branch.rs:429-430` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`、`docs/error-codes.md`|`command::branch_test::rename_missing_arg_exit_128`、`command::branch_test::rename_one_and_two_arg_behaviour`、共享回归组 `command::branch::tests::`|
| BR-09 | Issue #688 / `branch.rs:2282-2316` | `src/command/branch.rs`、`src/internal/reflog.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::rename_moves_reflog`、`command::branch_test::rename_partial_failure_rollback`、共享回归组 `internal::reflog::tests::`|
| BR-10 | Issue #689 / `branch.rs:2274-2280` | `src/command/branch.rs`、`src/internal/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::ref_namespace_df_conflict_create`、共享回归组 `internal::branch::tests::`|
| BR-34 | Issue #689 / `branch.rs:2274-2280` | `src/command/branch.rs`、`src/internal/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::ref_namespace_df_conflict_rename`、共享回归组 `internal::branch::tests::`|
| BR-35 | 已撤回 | 无写集 | 无 |N/A（本卡无独立测试命令）|
| BR-36 | Issue #689 / `branch.rs:2344-2350` | `src/command/branch.rs`、`src/internal/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::ref_namespace_df_conflict_copy`、共享回归组 `internal::branch::tests::`|
| BR-11 | Issue #690 / `branch.rs:2372-2384` | `src/command/branch.rs`、`src/internal/reflog.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::copy_preserves_reflog_and_all_config`、`command::branch_test::copy_partial_failure_rollback`、共享回归组 `internal::reflog::tests::`|
| BR-12 | Issue #691 / `branch.rs:2344-2350` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::copy_same_name_succeeds`、共享回归组 `command::branch::tests::`|
| BR-13 | Issue #692 / `branch.rs:348-349` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::copy_with_force_flag`、共享回归组 `command::branch::tests::`|
| BR-14 | Issue #693 / `branch.rs:480-482` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::short_ignore_case_flag`、共享回归组 `command::branch::tests::`|
| BR-15 | Issue #694 / `branch.rs:3012-3024` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::show_current_detached_silent`、共享回归组 `command::branch::tests::`|
| BR-16 | Issue #695 / `branch.rs:463-470` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::repeated_merged_filters`、共享回归组 `command::branch::tests::`|
| BR-17 | Issue #696 / `gh issue view 696` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::forked_branch_listing`、共享回归组 `command::branch::tests::`|
| BR-18 | Issue #699 / `branch.rs:3404-3416` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::sort_objecttype`、共享回归组 `command::branch::tests::`|
| BR-19 | Issue #700 / `branch.rs:477-478` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::repeated_sort_keys`、共享回归组 `command::branch::tests::`|
| BR-20 | Issue #700 / `for_each_ref.rs:1580-1615` | `src/command/for_each_ref.rs`、`src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/for-each-ref.md`、`docs/commands/zh-CN/for-each-ref.md`、`docs/development/commands/branch.md`、`docs/development/commands/for-each-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`、`COMPATIBILITY.md`|`command::for_each_ref_test::ahead_behind_atom`、`command::branch_test::format_ahead_behind_atom`、共享回归组 `command::for_each_ref::color_spec_tests::`|
| BR-21 | Issue #701 / `for_each_ref.rs:1391-1410` | `src/command/branch.rs`、`src/command/for_each_ref.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/for-each-ref.md`、`docs/commands/zh-CN/for-each-ref.md`、`docs/development/commands/branch.md`、`docs/development/commands/for-each-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`、`COMPATIBILITY.md`|`command::branch_test::format_includes_detached_head`、共享回归组 `command::for_each_ref_test::`|
| BR-22 | Issue #866 / `branch.rs:320-330,2171` | `src/command/branch.rs`、`src/internal/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::delete_remote_tracking_branch`、`command::branch_test::delete_remote_tracking_branch_rolls_back_midway_failure`、共享回归组 `internal::branch::tests::`|
| BR-23 | Issue #700 / `branch.rs:3341,3404` | `src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md`|`command::branch_test::sort_ahead_behind`、共享回归组 `command::branch::tests::`|
| BR-24 | Issue #866 / `src/command/fetch.rs` | `src/command/fetch.rs` |`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`、`COMPATIBILITY.md`|`command::fetch_test::skip_fetch_all`、共享回归组 `command::fetch::tests::`|
| BR-40 | Issue #684 / `util.rs:2649` | `src/utils/util.rs` |`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`|`command::rev_parse_test::push_transition_fail_closed_guards`|
| BR-25 | Issue #684 / `util.rs:2649` | `src/utils/util.rs` |`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`|`command::rev_parse_test::push_remote_and_upstream`、共享回归组 `command::rev_parse_test::push_transition_fail_closed_guards`|
| BR-29 | Issue #684 / `util.rs:2649` | `src/utils/util.rs`、`src/command/branch.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`|`command::rev_parse_test::push_explicit_branch_and_case`、`command::branch_test::branch_push_name_interpretation`|
| BR-27 | Issue #684 / `util.rs:2649` | `src/utils/util.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`|`command::rev_parse_test::push_simple_and_tracking`、共享回归组 `command::rev_parse_test::push_transition_fail_closed_guards`、共享回归组 `utils::util::test::`|
| BR-30 | Issue #684 / `util.rs:2649` | `src/utils/util.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`|`command::rev_parse_test::push_detached_head`、共享回归组 `utils::util::test::`|
| BR-26 | Issue #684 / `util.rs:2649` | `src/utils/util.rs` |`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`|`command::rev_parse_test::push_modes_and_refspec`、共享回归组 `command::rev_parse_test::push_remote_and_upstream`、共享回归组 `utils::util::test::`|
| BR-28 | Issue #684 / `for_each_ref.rs:250` | `src/utils/util.rs`、`src/command/for_each_ref.rs` |`docs/commands/for-each-ref.md`、`docs/commands/zh-CN/for-each-ref.md`、`docs/development/commands/for-each-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`、`COMPATIBILITY.md`|`command::for_each_ref_test::push_atom_custom_refspec_and_errors`、共享回归组 `command::for_each_ref_test::test_for_each_ref_push_atom`、共享回归组 `command::for_each_ref::color_spec_tests::`|

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划文本 Claude（Subagent）与 Codex 双审对同一字节均 PASS，基线冻结（`62abb622...`，`0.30.207`） | Claude `PASS` + Codex `PASS`（同一受审 SHA-256） | N/A |
| M1 | Phase 0（`BR-01`..`BR-05`）完成且发布 | 5 个 patch release + D 组全绿 | 按 `immutable-release` 前滚修复 |
| M2 | Phase 1 完成。发布切片是 `BR-06`..`BR-15`、`BR-31`、`BR-32`、`BR-34`、`BR-36`（含 `BR-10`；`BR-35` 不发布） | 14 个发布切片 + nextest 全绿 | 按 `immutable-release` 前滚修复 |
| M3 | Phase 2（`BR-16`..`BR-30` 及前置的 `BR-40`）及收口全量门全绿 | 全部 35 张活动卡 `done/complete`（不含已撤回的 `BR-33`、`BR-35`、`BR-37`、`BR-38`、`BR-39`） | 按 `immutable-release` 前滚修复 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| `branch -m` 迁移 reflog 中途失败 | 原分支与 reflog 完整保留 | 保持原分支不动，返回错误并提示重试 | 删除原分支但新分支无 reflog |
| `branch -r -d` 删除远程追踪分支失败 | 原远程追踪 ref 保留 | 回滚数据库事务，返回错误 | 误删同名本地分支或元数据 |
| `release.yml` 远端发布失败 | 已合并代码保留在 `main` | 按 ER-09/ER-10 新建 `FIX-*` 前滚修复 | 回退已推送 `main` 提交或删 tag |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `src/utils/util.rs` 修订与分支名解释影响面广 | 中：可能影响 `log`/`diff`/`checkout` 等命令 | 命中 ER-13 `T-1` 强制跑双环境完整 nextest | `BR-06`, `BR-32`, `BR-31`, `BR-07`, `BR-40`, `BR-25`, `BR-29`, `BR-27`, `BR-30`, `BR-26`, `BR-28` |
| 网站检出 `/Volumes/Data/gitmono/libra-backend` 与 `AGENTS.md:135` 记载不一致 | 低：路径已由用户 2026-11-18 11:55:09 UTC 确认，`AGENTS.md:135` 为已知文档滞后 | 每次写入前按「文档与网站验收命令」第 2 步核验，遇阻立即 STOP 报告 | 全部活动卡 |
| 创建、重命名或复制只交付退出码、不交付错误文本 | 中：用户看到非零退出但不知道碰撞的是哪条 ref | `BR-10`、`BR-34`、`BR-36` 各自把检查和 stderr 点名放在同一张可独立发布的卡里 | `BR-10`, `BR-34`, `BR-36` |
| 上游 `t3200#179` 依赖全局 `-C`（`#686`） | 低：仅影响外部完整脚本跑通 | 已登记 `DEP-BR-03`；本卡 focused 集成测试独立可跑 | `BR-17` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 分支列表与多键 `--sort` | $O(N \log N)$（$N$ 为分支数） | 单次调用线性预加载 commit 元信息 | 非验收观测目标：千分支规模，记录运行环境、样本数、median 与 spread；无数值通过门，G-10 仅禁止无界扫描/内存 | 非门禁观测，不设数值 PASS/FAIL；与测试矩阵 `Performance budget=N/A` 一致 |
| `%(ahead-behind:...)` 计算与排序 | 复用 `upstream_ahead_behind` | 按展示条目数线性调用 | 无无界全库扫描 | `command::for_each_ref_test` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已随各卡同步更新 `branch`、`for-each-ref`、`rev-parse`、`fetch` 行。
- [ ] `docs/commands/{branch,for-each-ref,rev-parse,fetch}.md` 与对应 `zh-CN/` 文档已同步。
- [ ] `docs/development/commands/{branch,rev-parse,for-each-ref,fetch}.md` 已同步。
- [ ] `docs/error-codes.md` 已核对/同步。
- [ ] `tests/INDEX.md` 与 `Cargo.toml` `[[test]]` 已核对（复用既有 `--test command_test`）。
- [ ] `plan-long.md` 日期计划索引已同步或确认 `N/A`。

## Codex review log

Result 只允许 `PASS` 或 `FAIL`。自 2026-11-18 双审起：一轮无 P0/P1 即记 `PASS`，P2 在 P2 处置列登记为残余风险；有 P0/P1 记 `FAIL` 并在 P0/P1 列写明条目，P0/P1 必须在下一轮复审关闭（ER-05）。针对历史字节的结论在 Scope 列注明，并在 Evidence 列说明其不适用于当前字节。被附证据驳回的发现在 P0/P1 列注明「驳回」与证据。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: 非法 `--test` 目标名；`#700` 缺 `--sort=ahead-behind`；`#866` 缺 `skipFetchAll` 及 clap 冲突诊断；`BR-17` 缺固定契约与 `DEP-BR-03`；`BR-06`/`BR-07` 缺分支名解释与 `revision`/`rev-parse` 文档；`BR-21` 缺 `for_each_ref.rs` 写集；`BR-20` 缺 `for-each-ref.en.md` | R2 全部同步关闭 | Claude/Codex R1 只读评审 |
| R2 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: `BR-17` `--forked` 契约与 Git `t3200` 不符；`BR-21` 缺 `detached from fromtag`；`Rollback mode` 应为 `immutable-release` 且缺 D 组远端门；`BR-05`/`BR-08` `error-codes.md` 需标 `T-1`；`BR-06`/`BR-07` 缺 `revision.en.md`；`BR-10` 需排除 `old_name`；`BR-13` 根因需含 `copy.num_args`；`BR-06` 拆出 `BR-25` 复用 `resolve_pushes`；外部上游脚本依赖登记 `DEFER-BR-02` | R3 全部同步关闭 | Claude/Codex R2 只读评审 |
| R3 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: `BR-25` 未同步到审计/追溯/M3/Phase 2；`base.yml` 误列 `network-remotes`；`immutable-release` 缺降级指引/兼容窗口；`BR-25` 需下沉 `resolve_pushes` 至 `util.rs`；`DEFER-BR-02` 缺具体责任人与 `#556` | R4 全部同步关闭 | Claude/Codex R3 只读评审 |
| R4 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: 审计表与 Phase 2 退出条件遗漏 `BR-25`；`BR-25` 缺 `for_each_ref_test` 验证与自定义 refspec/错误透传 AC；`BR-07` out-of-scope 误指 `BR-06` 为 `@{push}` 提供者 | R5 全部同步关闭 | Codex R4 只读评审 |
| R5 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: Phase 2 退出条件与局部仍写 24 卡/`BR-24`；`BR-25` `Granularity` 仍写 `AC=3/8; VER=2/8` 且漏 `for-each-ref` 文档影响；修订历史需按时间正序补全 | R6 全部同步关闭 | Claude/Codex R5 只读评审 |
| R6 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: `BR-25` 未覆盖未显式配置 `push.default`/`remote.<name>.push` 时按默认 `simple` + upstream + `origin` 解析 `@{push}`；P2: `M1`..`M3` 误写 `按单卡 revert 前滚`（Claude R6 遇到 `ECONNRESET` 网络中断） | R7 全部同步关闭 | Codex R6 只读评审 |
| R7 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: `BR-25` 默认 `simple` 契约与 Git `remote.c` `branch_get_push_1` 不符（漏同名/三角失败、`nothing`/`matching`、`@{p}`/`@{PUSH}`、`mirror` 与 `%(push)` 一致性）；P2: `ADR-BR-02` `revert` 措辞、G-03 谓词计数说明、`实施证据汇总` 时间戳 | R8 全部同步关闭 | Claude/Codex R7 只读评审 |
| R8 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: 移除非 Git `@{p}` 语法；修正 `simple` 跨远端三角失败语义；限定 detached HEAD 失败仅限从 `HEAD` 推断（显式 `<branch>@{push}` 正常解析）；`BR-25` 拆出 `BR-26` 对齐 `nothing`/`matching`/`current`/`refspec`/`mirror`/`tracking_for_push_dest`/`%(push)` 各 7/8 原子 AC | R9 全部同步关闭 | Claude/Codex R8 只读评审 |
| R9 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: `BR-25` 在 `BR-26` 落地前遇到 `remote.<r>.push`/`mirror` 须显式 fail-closed；P2: `评审结论` 依赖链至 `BR-26`（Claude R9 遇到 `ECONNRESET` 网络中断） | R10 全部同步关闭 | Codex R9 只读评审 |
| R10 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | 无 P0/P1；P2: 「成功定义」仍写 `25 张任务卡`（Claude R10 遇到 `ECONNRESET` 网络中断） | R11 全部同步关闭 | Codex R10 只读评审 |
| R11 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: `BR-25`→`25,27`、`BR-26`→`26,28` 拆分至原子 AC≤8；`BR-26` 补齐 `branch`/`revision` 文档；`BR-06` 补远程 `@{upstream}` 拒绝（`t3204#4`）；`BR-07` 明确 `branch.upstream-other.merge`；`BR-21` 补 `for-each-ref.md`；澄清 `#866` `t5514#9` 由 `BR-24` 完整覆盖 | R12 全部同步关闭 | Claude/Codex R11 只读评审 |
| R12 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R12 `FAIL` P1: `BR-25` 需对未配置 `push.default`（默认 `simple`）及 `nothing`/`matching`/`current` 在 `BR-27`/`BR-26` 落地前显式 fail-closed；`发布窗口顺序` 需展开 `BR-25 → BR-27 → BR-26 → BR-28`（Claude R12 `ECONNRESET`） | R13 全部同步关闭 | Codex R12 只读评审 |
| R13 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R13 `FAIL` P1: `BR-25` AC8 合并 4 种过渡配置导致实际谓词 11/8 超出 G-03 上限（Claude R13 `ECONNRESET`） | R14 全部同步关闭 | Codex R13 只读评审 |
| R14 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R14 `FAIL` P1: `BR-25` AC1 含 ref 与 commit 双谓词；缺显式 `push.default=simple` 过渡 AC；计划级 `T-1` 默认覆盖漏 `BR-29` | R15 全部同步关闭 | Codex R14 只读评审 |
| R15 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R15 `FAIL` P1: `BR-27` 依赖仍在 `BR-26` 的 `tracking_for_push_dest` 且缺 upstream/tracking 缺失独立错误；`BR-29` AC1 含 ref+commit 实为 `4/8`；`BR-17` 缺重复 `--forked` 并集语义；Claude R14 `FAIL` P1: `#684` 缺 `@{UPSTREAM}`/`@{U}` 大小写不敏感；P2: 同步 `评审结论`、`ADR-BR-02`、`BR-27`/`BR-26` 写集（Claude R15 `ECONNRESET`） | R16 全部同步关闭 | Codex R15 & Claude R14 只读评审 |
| R16 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R16 `FAIL` P1: `BR-06` detached HEAD 下应允许显式 `<branch>@{upstream}`；`BR-09`/`BR-11` 缺中间变更失败原子回滚/补偿 AC 与故障注入测试（Claude R16 `ECONNRESET`） | R17 全部同步关闭 | Codex R16 只读评审 |
| R17 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R17 `FAIL` P1: `BR-11` 回滚 AC 与测试漏目标分支 `metadata`（`MetadataKv`，`branch.rs:2392`）；`DEP-BR-08` 需明确为 `BR-01..BR-28` 计划级串行编辑锁（未获锁前阻塞编辑共享文件，接入 `BR-05`/`08`）且不预设 `plan-status.md` 已登记 | R18 全部同步关闭 | Codex R17 只读评审 |
| R18 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R18 `FAIL` P1: `DEP-BR-08` 锁区间漏 `BR-29`；网站文档路径须统一为 `/Volumes/Data/gitmono/libra-backend`（`cf` 分支，ER-06a） | R19 全部同步关闭 | Codex R18 只读评审 |
| R19 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: `DEP-BR-08` 须逐卡接入全部 29 张卡；`BR-25`/`29` 等 `Docs and compatibility impact` 须按 ER-06a 逐文件展开；P2: 修正 R18 修订历史措辞与 `EX-BR-01` 具名审批人 | R20 全部同步关闭 | Claude/Codex R19 只读评审 |
| R20 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: `BR-06` 需拆出 detached HEAD 规则并细化原子 AC（拆出 `BR-31`）；`BR-27` 需拆分未配置 `push.default` 与显式 `push.default=simple`（拆出 `BR-30`）；`BR-20`/`BR-21` 需按 ER-06a 展开完整逐文件文档清单（含 `BR-20` `for-each-ref.md`）；P2: 清理重复文档碎片、统一 `split-from` 谱系、修正 R19 日志 `P2` | R21 全部同步关闭 | Claude/Codex R20 只读评审 |
| R21 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: `BR-06` 拆出 `BR-32`（`@{u}` 与大小写）达到严格单谓词；全部卡补齐同卡 ER-06/ER-06a 文档交付验收门；`DEP-BR-08`/`发布窗口顺序`/追溯表（`BR-31`/`32`/`30` 及 `BR-27` 测试名）全量同步 32 卡；P2: 同步 `评审结论`、`#684` `1/9`..`9/9`、清理 `BR-28` 碎片与 `BR-01`/`14` AC 计数 | R22 全部同步关闭 | Claude/Codex R21 只读评审 |
| R22 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R22 `FAIL` P1: `BR-32`/`31` 拆分单谓词；Claude R22 `FAIL` P1: 补齐跨计划共享源码依赖 `DEP-BR-09..12`、网站路径改为本地可用的 `/Volumes/Data/gitmono/libra-backend`（`cf` 分支）；P2: `BR-30` 过渡说明、`BR-29` AC4、`BR-21` `%(refname)`、`EX-BR-01` R14 | R23/R24 全部同步关闭 | Claude & Codex R22 只读评审 |
| R24 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R24 `FAIL` P1: `DEP-BR-02` 需明确 `/Volumes/Data/gitmono/libra-backend` 与执行机 `/Volumes/Data/gitmono/libra-backend` 的授权核对规则；`BR-21` 需拆细 `%(HEAD)=*`/`%(refname)`/顺序/排除（Claude R24 `ECONNRESET`） | R25 全部同步关闭 | Codex R24 只读评审 |
| R25 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R25 `FAIL` P1: `BR-21` AC1/AC2 需拆细至严格单谓词（`8/8`）；移除无独立评审的 `R23` 日志占位行（Claude R25 `ECONNRESET`） | R26 全部同步关闭 | Codex R25 只读评审 |
| R26 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R26 `FAIL` P1: 网站交付必须固定为 `/Volumes/Data/gitmono/libra-backend` 且不可用则 STOP；`BR-27`/`BR-30`/`BR-26` 须同卡更新 `branch` 文档（Claude R26 `ECONNRESET`） | R27 全部同步关闭 | Codex R26 只读评审 |
| R27 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R27 `FAIL` P1: `BR-03` 复合 AC 超 8；`BR-27`/`30`/`26` 追溯表漏 `branch` 文档（Claude R27 `ECONNRESET`） | R28 全部同步关闭 | Codex R27 只读评审 |
| R28 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R28 FAIL P1: `BR-03` 豁免缺逐门清单；追溯表测试名未同步；`BR-07` 谓词少计（Claude 未跑） | 后续修订关闭 | Codex R28 只读评审 |
| R29 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R29 FAIL P1: `BR-03` 取值列表超门族；`BR-09` reflog 首条 AC 少计；修订历史缺 R28 行（Claude 未跑） | `2026-11-17 08:28:29 UTC` 修订关闭 | Codex R29 只读评审 |
| R30 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R30 FAIL P1: `BR-29` 缺分支名槽 `{@{push}}`；`BR-10` 谓词少计；`BR-20`/`BR-21` 落点应按 `src/command/` 计为 1（Claude 未跑） | `2026-11-17 09:06:33 UTC` 修订关闭 | Codex R30 只读评审 |
| R31 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R31 FAIL P1: `EX-BR-02` 把列对齐并进着色门族（Claude 未跑） | `2026-11-17 09:36:28 UTC` 拆出 `BR-33` 并撤回豁免 | Codex R31 只读评审 |
| R32 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R32 FAIL P1: `BR-33` 事实基线与源码矛盾；Phase 2 退出条件未覆盖撤回后的范围（Claude 未跑） | `2026-11-17 09:51:20 UTC` 撤回 `BR-33` | Codex R32 只读评审 |
| R33 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R33 FAIL P1: `BR-10` 把四条 stderr 点名算成一条，实际超过 8（Claude 未跑） | `2026-11-17 09:57:57 UTC` 拆出 `BR-34` | Codex R33 只读评审 |
| R34 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R34 FAIL：谓词超限后曾改为 `REL-BR-10`（Claude 未跑） | `2026-11-17 10:43:14 UTC` 撤回家族 | Codex R34 只读评审 |
| R39 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R39 FAIL P1: `REL-BR-10` 没有不可分割理由，且把冲突检查与错误文本拆开（Claude 未跑） | `2026-11-17 10:43:14 UTC` 按创建/重命名/复制重切并撤回 `BR-35` | Codex R39 只读评审 |
| R40 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R40 FAIL P1: `BR-36` 缺反向 D/F；测试矩阵未列出三条指定测试（Claude 未跑） | `2026-11-17 10:52:35 UTC` 关闭 | Codex R40 只读评审 |
| R41 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R41 FAIL P1: `BR-03` 的 `auto` 未按终端区分（Claude 未跑） | `2026-11-17 11:14:03 UTC` 拆出 `BR-37` | Codex R41 只读评审 |
| R42 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R42 FAIL P1: `DEP-BR-08` 与 Phase 2 未列入 `BR-37`（Claude 未跑） | `2026-11-17 11:19:54 UTC` 关闭 | Codex R42 只读评审 |
| R43 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R43 FAIL P1: `BR-03` 发布后 `auto` 没有契约（Claude 未跑） | `2026-11-17 11:28:33 UTC`：`BR-03` 把 `auto` 做成 `never`，`plain` 改由 `BR-37` 交付 | Codex R43 只读评审 |
| R44 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R44 FAIL P1: `auto` 不能先发成 `never`；`plain` 与终端轴混卡（Claude 未跑） | `2026-11-17 11:38:06 UTC`：`BR-03` 一次交付完整 `auto`，`BR-37` 只交付五个颜色槽 | Codex R44 只读评审 |
| R45 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: 颜色级联未保留 machine/NO_COLOR/TERM=dumb 且缺省与显式 auto 不可区分；普通/unborn/detached/column 渲染覆盖不全。已补显式 argv 传递、全局优先级、四路径验收并同步写集/粒度。该轮未完成全文复审，R46 不得据此推定其它维度通过 | R46 完整复审 | Codex R45 只读评审（部分覆盖） |
| R46 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: 四路径与禁色条件合并导致 AC 计数失真、M0 仍双审、默认 plain 误要求 ANSI。已新增 `BR-38` 按路径验收 Git parity、拆分禁色谓词、M0 改 Codex 单审并保留 plain 默认无 ANSI。该轮未完成全文复审，R47 不得据此推定其它维度通过 | R47 完整复审 | Codex R46 只读评审（部分覆盖） |
| R47 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: 三张颜色卡各自发布会留下未验收中间态；P2: 实施证据时间陈旧。已新增 `REL-BR-03`/`BR-39` 统一首次发布并同步时间。该轮未完成全文复审，R48 不得据此推定其它维度通过 | R48 完整复审 | Codex R47 只读评审（部分覆盖） |
| R48 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: `ADR-BR-02` 与完成判据仍要求全部卡独立发布；P2: 家族窗口和逆序恢复不完整。已统一为子卡继承 `BR-39` C/D，并补齐禁止插入项、发布前逆序恢复与发布后前滚。该轮未完成全文复审，R49 不得据此推定其它维度通过 | R49 完整复审 | Codex R48 只读评审（部分覆盖） |
| R49-A/B (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: 全局条款仍逐卡发布、共享锁与家族窗口冲突、发布禁令误伤 `BR-39`、跨计划 `WC-10`/`PM-21` 顺序相反。已分别修正全局发布范围、家族锁、子卡阶段禁令及统一顺序。R50 分区复核 | R50-A/B 分区完整复审 | Codex R49-A/B 只读评审（分区覆盖） |
| R50-A (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: 缺 `--no-color` 覆盖强制着色测试，共享 OutputConfig 修改缺非 branch 回归。已在同 AC/VER 内补 `--no-color` 和 OutputConfig/非 branch 测试并纳入 `BR-39` 聚合门。R51-A 分区复核 | R51-A 分区完整复审 | Codex R50-A 只读评审（分区覆盖） |
| R50-B (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: `BR-25` 未约束 upstream 跨 remote、`BR-22` 缺中途失败回滚；P2: 200ms 预算不可测。已补同 remote 成功/跨 remote 失败、删除故障注入与 AC/VER 计数，性能改为非门禁观测目标。R51-B 分区复核 | R51-B 分区完整复审 | Codex R50-B 只读评审（分区覆盖） |
| R51-A (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: `BR-09` 缺 `-M` 覆盖目标失败恢复；P2: `BR-11` 缺 copy 记录断言。已补目标状态故障注入与 copy 记录验证并同步 AC/VER。R52-A 分区复核 | R52-A 分区完整复审 | Codex R51-A 只读评审（分区覆盖） |
| R51-B (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: 同 remote 成功与跨 remote 失败合并导致 `BR-25` 实计 10 项。已将 7 项过渡守卫拆至 `BR-40` 并前置，`BR-25` 仅保留 3 项 upstream/remote 谓词，取消 `EX-BR-01`。R52-B 分区复核 | R52-B 分区完整复审 | Codex R51-B 只读评审（分区覆盖） |
| R52-A (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: `DEP-BR-09` 漏 `BR-40`；`BR-09` 的 `-M` 覆盖故障注入缺少可执行成功前置。已纳入锁清单并由本卡交付 `-M` 成功路径及成功/失败双测试。R53-A 分区复核 | R53-A 分区完整复审 | Codex R52-A 只读评审（分区覆盖） |
| R52-B (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: `BR-40` 缺最小 `@{push}` 解析入口，无法区分各配置拒绝；P2: 文档清除责任仍指旧卡。已补最小入口、7 个可区分拒绝和 `BR-40` 清除责任。R53-B 分区复核 | R53-B 分区完整复审 | Codex R52-B 只读评审（分区覆盖） |
| R54-A (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: 普通 `-m` 成功路径未要求迁移配置，可能丢失追踪行为。已补全量配置迁移、旧配置清理与成功路径断言，AC=8/8、VER=5/8。R55-A 分区复核 | R55-A 分区完整复审 | Codex R54-A 只读评审（分区覆盖） |
| R54-B (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: `BR-40` 独立发布未规定 upstream 过渡行为。已增加 `push.default=upstream` fail-closed 判据及测试（AC=8/8），并要求 `BR-25` 同卡移除。R55-B 分区复核 | R55-B 分区完整复审 | Codex R54-B 只读评审（分区覆盖） |
| R55-A (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: `-M` 成功配置迁移缺少可验证合同。已明确源配置迁移、覆盖前目标配置和旧名称配置清理及逐项断言。R56-A 分区复核 | R56-A 分区完整复审 | Codex R55-A 只读评审（分区覆盖） |
| R55-B (Codex gpt-6.1-terra) | `plan-20261117-branch.md` 第 1001 行至文件末及指定交叉引用（历史字节） | PASS | 无 P0/P1/P2。已核对 `BR-40` upstream 过渡拒绝与 `BR-25` 清除责任、`BR-22` 回滚及性能非门禁表述 | 无 | Codex R55-B 只读评审（分区覆盖，历史字节结论；路径与追溯修订后不适用于当前字节，须重新完成当前文件全部分区复审） |
| R56-A (Codex gpt-6.1-terra) | `plan-20261117-branch.md` 第 1–1000 行、完成判据与 `DEP-PM-06`（历史字节） | PASS | 无 P0/P1/P2。普通 `-m`/`-M` 配置迁移与恢复、`DEP-BR-09`、`REL-BR-03` 均已核对 | 无 | Codex R56-A 只读评审（分区覆盖，历史字节结论；与 R55-B 共同构成当时字节的单审通过，路径与追溯修订后不适用于当前字节，须重新完成当前文件全部分区复审） |
| R53-A (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: 普通 `-m` 恢复遗漏源/目标 config。已把 `branch.<name>.*` 操作前快照与逐项恢复断言加入 AC 和故障注入测试。R54-A 分区复核 | R54-A 分区完整复审 | Codex R53-A 只读评审（分区覆盖） |
| R53-B (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P2: 风险表遗漏 `BR-40`，且 `BR-25` 描述仍像重新新增解析器。已补风险关联卡并明确在 `BR-40` 入口上扩展。本轮未完成全分区复审，R54-B 不得据此推定其它维度通过 | R54-B 分区完整复审 | Codex R53-B 只读评审（部分覆盖） |
| R1 (Codex gpt-6.1-terra，A–F 六分区) | `plan-20261117-branch.md` 全文分区（受审 SHA-256 `2c84d2c49b5c…`） | FAIL | P1：复合谓词少计 G-03（`BR-01/02/03/08/09/11/13/14/15/25/29`）；`BR-24` 审计/追溯行误写；撤回存根仍写 38 张；`BR-07` AC5/AC6 夹具冲突与描述超范围、AC2–AC4 映射漏退出码；`BR-05` 两键删除未断言；`BR-32` 显式分支名路径未测；`BR-11` reflog 全行比较不可通过；`BR-19` 夹具不能区分次键；`BR-17` 描述超范围；`BR-22` 全名拒绝无 AC；`BR-25` 优先级/回退未区分且回退规则漏唯一远端；`BR-29` 超限；`BR-40` 前置混入行为；`BR-02` 多通配与 `rev-parse` 成功判定；`BR-03` 声称 `-vv` 无验收。R2 全部处理（修订历史 R1 行） | `BR-03` 字节串歧义已改 `\x1b` 字面量 | Codex R1 六分区只读评审（`r1-codex-A..F`） |
| R1 (Claude subagent claude-opus-5-5) | `plan-20261117-branch.md` 全文（受审 SHA-256 `2c84d2c49b5c…`） | FAIL | P1：`revision.md` 为 `libra revision` 命令页、不应承载修订语法；`BR-07` 夹具冲突与描述超范围；`BR-17` 描述超范围；`BR-24` 行误写；存根 38 张。R2 已处理 | 同 Codex P2 | Claude R1 全文评审 |
| R2 (Codex gpt-6.1-terra，A–F 六分区) | `plan-20261117-branch.md` 全文分区（受审 SHA-256 `e7352ef5032e…`） | FAIL | P1：`BR-03` 落点计数；`BR-02` start-point 不可区分；`BR-07` `--abbrev-ref` 路径不在写集、AC6 不能证明键不存在；`BR-09`/`BR-11` 按键计数超限与固定键夹具、reflog 只比三字段；`BR-12` 只比 message；`BR-21` AC6/AC7 复合；`BR-18` 缺 `#699` 配置排序场景；`BR-23` 缺 `t3203` 形态；多卡缺 issue 指定的上游单测/TC 门；`BR-40` 拒绝不可区分；`BR-29` 声称 `@{PuSH}` 无验收。R3 全部处理（修订历史 R2 行） | `BR-14` 有序输出（P2）已处理 | Codex R2 六分区只读评审（`r2-codex-A..F`） |
| R2 (Claude subagent claude-opus-5-5) | `plan-20261117-branch.md` 全文（受审 SHA-256 `e7352ef5032e…`） | FAIL | P1：`DEP-BR-08` 漏 `BR-08` 的 `error-codes.md` 写入；`DEP-BR-10` 未覆盖 `fetch_test.rs`/fetch 文档与 `global-init-infra`。R3 已处理 | 无 | Claude R2 全文评审 |
| R3 (Codex gpt-6.1-terra，A–F 六分区) | `plan-20261117-branch.md` 全文分区（受审 SHA-256 `e75fc568e4ec…`） | FAIL | A、C 分区无 P0/P1（PASS）；P1：`BR-01` `--column=auto` 非终端与 Git 不符；`BR-03` 描述填充范围与 AC 冲突；`BR-09`/`BR-11` reflog 漏 `worktree_id`、成功路径无 ref 判据、故障注入不能证明命中；`BR-34` 成功场景只判退出码；`BR-21` tag 名 detached-from 需写集外的 `switch.rs`；`BR-26` mirror 夹具不可区分；`BR-28` 与既有 `test_for_each_ref_push_atom` 冲突。R4 全部处理 | `BR-40` 摘要「7 项」、`BR-23` 覆盖标注（P2）已处理 | Codex R3 六分区只读评审（`r3-codex-A..F`） |
| R3 (Claude subagent claude-opus-5-5) | `plan-20261117-branch.md` 全文（受审 SHA-256 `e75fc568e4ec…`） | FAIL | P1：`BR-18` 夹具 `libra config branch.sort -refname` 实测 exit 129。R4 已处理 | `BR-40` 整句模板（P2）已处理 | Claude R3 全文评审 |
| R4 (Codex gpt-6.1-terra，A–F 六分区) | `plan-20261117-branch.md` 全文分区（受审 SHA-256 `5f156fc2c226…`） | FAIL | A、C 分区无 P0/P1（PASS）；P1：`BR-01` 终端 `--column=auto` 声称无验收；`BR-09`/`BR-11` 配置比较未含 `encrypted` 且集合丢失多值次序与重复；`BR-22` 故障注入不能证明命中且不能覆盖删除顺序；`BR-28` 既有用例夹具缺 `remote.<r>.fetch`。R5 全部处理 | 无 | Codex R4 六分区只读评审（`r4-codex-A..F`） |
| R4 (Claude subagent claude-opus-5-5) | `plan-20261117-branch.md` 全文（受审 SHA-256 `5f156fc2c226…`） | PASS | 无 P0/P1 | P2：FI 的 `br-fi` 依赖错误包装保留底层消息；`BR-25` 依赖守卫的预期由 `BR-26` 改写（均已在卡内写明，登记为残余风险） | Claude R4 全文评审 |
| R5 (Codex gpt-6.1-terra，A–F 六分区) | `plan-20261117-branch.md` 全文分区（受审 SHA-256 `5e728616a48b…`） | FAIL | A、C 分区无 P0/P1（PASS）；P1：`BR-02` 缺 `#679` 的 `--list -v` 验收；`BR-09`/`BR-11` 声称全部配置而加密行已延后；`BR-22` 全名拒绝在 ref 已删后执行；`BR-28` 既有用例「互不相同」与三角空串冲突。R6 全部处理 | 无 | Codex R5 六分区只读评审（`r5-codex-A..F`） |
| R5 (Claude subagent claude-opus-5-5) | `plan-20261117-branch.md` 全文（受审 SHA-256 `5e728616a48b…`） | FAIL | P1：`BR-22` 按 `name='feat'` 识别远程追踪 ref 行，与实际存储形式不符。R6 已处理 | 同 R4 残余风险 | Claude R5 全文评审 |
| R6 (Codex gpt-6.1-terra，A–F 六分区) | `plan-20261117-branch.md` 全文分区（受审 SHA-256 `8e508ff78f5a…`） | FAIL | B、C、E 分区无 P0/P1（PASS）；P1：审计表 `BR-11`「全量配置」与加密延后冲突；`BR-09`/`BR-11` 夹具不能检出目标残留（处理：Git 本身保留孤立目标配置，改为前置 + `DEFER-BR-10`）；`BR-13` 等价只验提交；改写的既有用例分类与协议冲突。R7 全部处理 | P2（残余风险，不修）：`BR-02` 多模式只测两个；`BR-01` 未单测 `-vv`；`BR-17` 夹具 `main` 初始关系可更明确 | Codex R6 六分区只读评审（`r6-codex-A..F`） |
| R6 (Claude subagent claude-opus-5-5) | `plan-20261117-branch.md` 全文（受审 SHA-256 `8e508ff78f5a…`） | PASS | 无 P0/P1 | 同 R4 残余风险 | Claude R6 全文评审 |
| R7 (Codex gpt-6.1-terra，A–F 六分区) | `plan-20261117-branch.md` 全文分区（受审 SHA-256 `cad7debafe55…`） | PASS | 无 P0/P1（六分区均 PASS） | P2（残余风险，不修）：`BR-03` AC2/AC3 映射允许多余输出行；`BR-17` 夹具未写明本地 `main` 与远端 `base` 的关系；`BR-29` AC6 只验 `branch.main.merge` | Codex R7 六分区只读评审（`r7-codex-A..F`） |
| R7 (Claude subagent claude-opus-5-5) | `plan-20261117-branch.md` 全文（受审 SHA-256 `cad7debafe55…`） | FAIL | P1：`BR-13` 对照仓库分别建库导致提交 ID 不同，等价断言不可通过。R8 已处理 | 同 R4 残余风险 | Claude R7 全文评审 |
| R8 (Codex gpt-6.1-terra，差异确认) | `plan-20261117-branch.md`（受审 SHA-256 `ea135d1e7ecdf0db6e6af27159786b1b828e8b14d41969f06650df92c3651832`；相对 R7 全文 PASS 字节 `cad7debafe55…` 仅 `BR-13` 与记账行差异） | PASS | 无 P0/P1 | 无新增 P2（R4/R7 残余风险见上） | Codex R8 只读差异确认（`diff` 核对 + `BR-13` 全卡复核） |
| R8 (Claude subagent claude-opus-5-5，差异确认) | 同上 | PASS | 无 P0/P1 | 残余风险：FI 的 `br-fi` 依赖错误包装保留底层 SQLite 消息；`BR-25` 依赖守卫的预期由 `BR-26` 改写；R6/R7 Codex P2（`BR-01` `-vv`、`BR-02` 多模式、`BR-03` 多余行、`BR-17` 夹具关系、`BR-29` AC6） | Claude R8 差异确认 + 结构脚本复核 |

## 实施证据汇总

- 计划成稿时间：`2026-11-17 02:53:00 UTC`；历史原记录的最新修订时间为 `2026-11-17 12:27:28 UTC`（R46–R56 的实际发生时间与登记性质待核实）；2026-11-18 起的修订与双审轮次见「修订历史」与「Codex review log」；仓库基线 `main@62abb622f0eb2626934633ada78061963ebf1f44`；`Cargo.toml` 版本 `0.30.207`。
- 后续各卡的 focused 测试计数、nextest 全量摘要、`release.yml` run ID 与 D 组结果将直接登记于本节。

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-BR-01 | 无（21 个分配 issue 均有承接卡：`BR-01`..`BR-32`、`BR-34`、`BR-36`、`BR-40`；`#680` 由 `BR-03` 按 issue 验收项交付，超出部分见 `DEFER-BR-03`） | N/A | N/A | N/A |
| DEFER-BR-02 | 外部上游 Git shell 脚本与 TC 门：各 issue 验收项中的「pinned 上游单个测试通过 / 对应 TC 通过」（如 `#699` 的 `t3200#164`/TC-0295、`#700` 的 `t3203#33`、`#701` 的 `t3203#31`/TC-0304、`#696` 的 `t3200#179`/TC-0290、`#866` 的 `t5514#9`/TC-1351 等）以及 `t3200`/`t3203`/`t3204`/`t5514` 整套 harness 全量跑通。本计划各卡以同形 focused 用例交付这些 issue 的行为验收项；上游脚本/TC 验收项不在本计划完成门内，相关 issue 在本计划完成后为部分闭环，须保持 open 直至本项完成 | 整套外部脚本运行受外部前置 `DEP-BR-03`（`#686`）、`DEP-BR-04`（`#676`）、`DEP-BR-05`（`#541`）、`DEP-BR-06`（`#675`）、`DEP-BR-07`（`#556`）阻塞 | 上述前置合入后由 CLI / Git-compat 维护者执行上游回归门 | 外部上游兼容性回归计划（Owner: CLI / Git-compat 维护者） |
| DEFER-BR-03 | `#680` 之外的颜色轴：`color.branch`/`color.ui` 模式级联、显式 `--color=auto` 与缺省区分、`NO_COLOR`/`TERM=dumb`/`--machine` 与 `color.branch` 的优先级、`color.branch.upstream`/`plain` 槽、unborn/detached HEAD/`--column` 着色路径（原 `BR-37`/`BR-38`/`BR-39` 范围） | 超出 `#680` 明确验收项（issue 只要求 `--color -v -a` 下 current/local/remote 槽与名称填充）；且 Git 对未出生分支不列出该分支（pinned Git 实测 `git checkout --orphan newb && git branch` 只列 `main`），而 Libra 列出 `* main`，属另一行为差异 | 有对应 issue 或用户要求时 | 新 issue/后续计划（Owner: CLI 维护者） |
| DEFER-BR-04 | `branch -M` 强制重命名及其覆盖目标时的 reflog/`branch.*` 配置迁移与失败恢复（原 `BR-09` AC6/AC8） | `BranchArgs` 无 `-M`（`src/command/branch.rs:429-440` 只有 `-m`/`-c`/`-C`），属 `#675` 的独立行为轴；`#688` 只要求 `-m` 迁移 reflog | `#675` 排期时 | `#675`（Owner: CLI 维护者） |
| DEFER-BR-05 | `branch -m` 超过 2 个参数的退出码与文案：Git 为 exit 128 + `fatal: too many arguments for a rename operation`（pinned Git 实测），Libra 0.30.207 实测为 clap 用法错误 exit 129（`LBR-CLI-002`，`the argument '--move <OLD_BRANCH> <NEW_BRANCH>' cannot be used with '[NEW_BRANCH]'`）；`BR-08` 不对超过 2 个参数的情形作任何断言 | `#687` 只要求 0 参数时 exit 128 + `branch name required` | 有对应 issue 时 | 新 issue（Owner: CLI 维护者） |
| DEFER-BR-06 | `branch -C <old> <existing>` 覆盖已有目标时的 `branch.<existing>.*` 配置与 reflog 处理：pinned Git 实测保留目标原配置段并追加源配置段（`git config --get-regexp` 同时列出两段），目标 reflog 被源 reflog 副本加一条复制记录替换；Libra `config_kv` 的多值语义需另行设计（原 `BR-11` AC4/AC5 的 `-C` 部分） | `#690` 只要求 `--copy` 向新分支复制 reflog 与配置 | 有对应 issue 时 | 新 issue（Owner: CLI 维护者） |
| DEFER-BR-07 | `branch --forked <remote>`（经 `<remote>/HEAD` 选定，pinned Git `t/t3200-branch.sh:1860-1865`）、`@{-N}` 在 `branch` 创建/`-f` 分支名参数中的解释，以及自 tag 名分离后的 `(HEAD detached from <tag>)`（需 `switch`/`checkout` 在 HEAD reflog 保留输入名，`src/command/switch.rs` 现只记短对象 ID） | 超出 `#696`（只要求 `--forked <pattern>` 与 `--no-merged`/`--format` 组合）与 `#685`（只要求 `--set-upstream-to`/`--unset-upstream`）的明确验收项；tag 名形态需修改本计划写集外的导航命令（`#701` 的 `t3203#31` 全量期望另由 `DEFER-BR-02` 承接） | 有对应 issue 时 | 新 issue（Owner: CLI 维护者） |
| DEFER-BR-08 | 终端（stdout 为 TTY）下 `libra branch --column=auto -v` 以 128 拒绝的验收 | 需 PTY 测试夹具，现有子进程 helper 只捕获管道（`tests/command/mod.rs:385-389`）；`#678` 只要求 `--column -v` 拒绝与单独使用可用 | 引入 PTY 测试 helper 时 | 后续计划（Owner: CLI 维护者） |
| DEFER-BR-09 | 加密（`config_kv.encrypted = 1`）的 `branch.<name>.*` 配置在 `-m`/`-c` 时的迁移与复制（密文与 vault 语义需另行设计） | `#688`/`#690` 只涉及普通配置（`branch.copy.dummy`）；加密配置依赖 vault（`libra config set --encrypt`） | 有对应 issue 时 | 新 issue（Owner: CLI 维护者） |
| DEFER-BR-10 | 目标名已存在孤立 `branch.<new>.*` 配置（无同名分支）时 `-m`/`-c` 的配置合并：pinned Git 实测保留孤立段并与迁移/复制来的段并存（`git branch -m topic renamed` 后 `branch.renamed.orphan` 仍在） | `#688`/`#690` 未涉及；`BR-09`/`BR-11` 夹具以「执行前不存在目标配置」为前置 | 有对应 issue 时 | 新 issue（Owner: CLI 维护者） |

## 完成判据

计划只有在以下条件全部满足后才能标记完成：

- [ ] 所有任务卡满足粒度规则 `G-*`：无未登记的 L 例外、无 XL 卡、无碎片卡、无未登记的合并发布例外、实现写集冲突均已消解；「任务卡粒度审计表」已填齐。
- [ ] 全部 35 张活动卡（`BR-01`..`BR-32`、`BR-34`、`BR-36`、`BR-40`）的 acceptance criteria 已满足，且 `Lifecycle=done` **且** `Acceptance=complete`（ER-04）；已撤回的 `BR-33`、`BR-35`、`BR-37`、`BR-38`、`BR-39` 与 `DEFER-BR-02`..`DEFER-BR-10` 所列内容不在完成门内；各 issue 的行为验收项由承接卡交付，issue 中「上游单个测试 / TC 通过」类验收项由 `DEFER-BR-02` 承接，故本计划完成时这些 issue 只能部分闭环、保持 open，直至 `DEFER-BR-02` 完成。
- [ ] 所有活动卡的 Verification 命令已按「验证执行协议」运行并记录结果。
- [ ] 网站交付门：每张活动卡完成「文档与网站验收命令」第 1–7 步（变更分支含独立 Claude Code 字面 `VERDICT: PASS`、精确暂存、签名+DCO 提交、提交内容与受审字节一致、推送 `cf` 与页面检查；无变更分支含页面检查）；任一环节失败即 STOP 且不得标 `complete`。
- [ ] **计划完成门 / 全量收口门（ER-13）**：在全部活动卡完成后运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，三者全绿；每次运行记录 `libra rev-parse HEAD` 与最终代码树（`docs/development/plan/` 以外路径）内容；其后仅 `docs/development/plan/` 下的记账提交不触发重跑，其它路径变化即须在新树上重跑。
- [ ] **全量收口门暴露的 Bug 已全部处理（ER-13）**：定位到本计划的失败已前滚修复并在最终代码树上重跑全量至全绿；判定为既有失败的用例已附「计划基线 commit 上同样失败」的复现证据并登记 `FIX-*` 或 `DEFER-*`。
- [ ] 必要的 docs/compat/error-code/test-index 更新已完成。
- [ ] 每张活动卡的 `Rollback mode` 均已验证或记录。
- [ ] 计划文本 Claude（Subagent）与 Codex 双审对同一受审字节均 `PASS`，P0/P1 全部关闭（用户 2026-11-18 指令）；执行期每卡 ER-05 评审另行闭环。
- [ ] 每张活动卡均已完成 `patch + 1`、版本面同步、工具链刷新 `Cargo.lock`、`gh release create v<version>` 触发 `release.yml` 并取得 Cloudflare 发布证据（ER-08）；已撤回卡不发布。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
- [ ] `plan-long.md` 相关状态或日期计划索引已同步，或明确 `N/A`。
