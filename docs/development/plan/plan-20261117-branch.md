# `libra branch` Git 兼容与缺陷修复计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：Codex R56-A 与 R55-B 分区均 PASS，计划可提交；全部任务 `pending`，尚未执行。最近修订 `2026-11-17 12:27:28 UTC`。本计划不在 v2.16 `code-only` 具名范围内，执行标准 `patch + 1` + `gh release create` 发布合同。按用户 2026-11-17 指令，计划提交门为 Codex 单审，不再要求 Claude Review。
> 覆盖 Open Issues（21 个）：`#678`、`#679`、`#680`、`#681`、`#682`、`#684`、`#685`、`#687`、`#688`、`#689`、`#690`、`#691`、`#692`、`#693`、`#694`、`#695`、`#696`、`#699`、`#700`、`#701`、`#866`。

## 文档职责

本文解决 `libra branch` 及关联通用修订解析（`@{upstream}` / `@{push}` / `@{-N}`）、`for-each-ref` 格式原子（`%(ahead-behind:...)` / detached HEAD）与 `fetch --all`（`skipFetchAll`）在 Git 兼容测试中的 21 项已知缺陷，目标是逐卡交付可独立验收、可独立发布、附带单元/集成测试与中英文/网站文档同步的修复切片。

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
- 全部 38 张活动卡（`BR-01`..`BR-32`、`BR-34`、`BR-36`..`BR-40`；不含已撤回的 `BR-33` 与 `BR-35`）达到 `Lifecycle=done` 且 `Acceptance=complete`，全部卡与收口全量门及 Codex review 均通过。

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
| 文档 | `docs/commands/{branch,for-each-ref,revision,rev-parse,fetch}.md`（逐文件同步 EN 与 zh-CN）、`docs/development/commands/branch.md`、网站 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/{branch,for-each-ref,rev-parse,fetch}.en.md`、`COMPATIBILITY.md` | `docs/commands/branch.md`、`COMPATIBILITY.md:206` |
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
| 同批并行计划（`clone` / `describe` / `merge-pull` / `global-init-infra` / `fetch-push` / `checkout-switch-restore` / `worktree` / `plumbing-misc` / `grep` / `diff`） | 跨计划共享写集 | `src/utils/util.rs`（`BR-06`/`32`/`31`/`07`/`40`/`25`/`29`/`27`/`30`/`26`/`28` 与 `clone`/`describe`/`merge-pull`/`global-init-infra` 共享，`DEP-BR-09`）；`src/command/fetch.rs`（`BR-24` 与 `fetch-push`/`clone` 共享，`DEP-BR-10`）；`src/command/branch.rs`（与 `CSR-10` 共享，`DEP-BR-11`）；`src/internal/branch.rs`（`BR-10`/`BR-34`/`BR-36`/`BR-22` 与 `worktree#WC-10`/`plumbing-misc#PM-21` 共享，`DEP-BR-12`）；`COMPATIBILITY.md`/`docs/error-codes.md`（与 `grep`/`checkout-switch-restore`/`diff` 共享，`DEP-BR-08`），全部由协调者串行调度（G-10） |
| `plan-status.md` | 全仓计划单一视图 | 本计划建稿由协调者统一登记；执行期每卡推进同批更新 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 21 个 `branch` 兼容缺陷覆盖 `t3200`/`t3203`/`t3204`/`t5514` 等场景，值得按单轴切片逐一收敛 | 无 |
| 可行性 | 落点集中在 `branch.rs`、`internal/branch.rs`、`internal/reflog.rs`、`util.rs`、`for_each_ref.rs`、`fetch.rs` | 拆分为 38 张活动卡（`BR-01`..`BR-32`、`BR-34`、`BR-36`..`BR-40`；`BR-33` 与 `BR-35` 撤回） |
| 任务卡粒度 | `#700` 拆为 `BR-19`（重复 `--sort`）、`BR-20`（`%(ahead-behind)` 原子）、`BR-23`（`--sort=ahead-behind`）；`#866` 拆为 `BR-22`（`-r -d`）与 `BR-24`（`skipFetchAll`）；`#684` 拆为 `BR-06`/`BR-32`/`BR-31`/`BR-40`/`BR-25`/`BR-29`/`BR-27`/`BR-30`/`BR-26`/`BR-28`（`BR-40` 先交付 7 项过渡 fail-closed 守卫，`BR-25` 仅保留 upstream 与 remote 选择） | 全部卡满足 `S`/`M`、AC≤8、VER≤8；`EX-BR-01` 已取消。基线已对齐 `-v` 列宽，`BR-33` 不再实施 |
| 依赖与顺序 | 按 Phase 0 -> Phase 1 -> Phase 2 严格对齐线性链 `BR-01 -> ... -> BR-28`，并包含后拆出的 `BR-34`、`BR-36`..`BR-40`（不含已撤回的 `BR-33`、`BR-35`） | 无并发 I–I 冲突 |
| 完整性 | R1 补齐了 `--test command_test` 目标名、分支名解释接线、`skipFetchAll`、`--sort=ahead-behind` 与 `for-each-ref.en.md`/`revision.md`/`rev-parse.md` 文档 | 无遗漏 |
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
| 2026-11-17 05:03:00 UTC | Codex R18 FAIL | 将 `DEP-BR-08` 覆盖范围统一至全部 29 卡（`BR-01..BR-29`，含 `BR-29`）并改为逐卡获取/释放串行编辑锁；将全文网站文档路径 `/Volumes/Sea/libra-backend` 统一为 `/Volumes/Sea/libra-backend`（`cf` 分支，按 ER-06a 不可用则 STOP） | N/A | 全文 |
| 2026-11-17 05:12:00 UTC | Claude & Codex R19 FAIL | 将 `DEP-BR-08` 同步接入全部 29 张任务卡的 `Dependencies`、`Granularity` 与审计表；展开所有卡 `Docs and compatibility impact` 的逐文件清单与 ER-06/ER-06a 逐文件验收门；`EX-BR-01` 审批人具名为 `Eli（仓库维护者 / Plan Owner）` | N/A | waiver 表、审计表、全部 29 张卡、实施证据汇总、review log |
| 2026-11-17 05:15:00 UTC | Claude & Codex R20 FAIL | 将 `BR-06` 拆出 `BR-31`（detached HEAD `@{upstream}` 规则，`3/8`），`BR-06` 细化为 6 条独立单谓词 AC（`6/8`）；将 `BR-27` 拆出 `BR-30`（detached HEAD `@{push}` 规则，`2/8`），`BR-27` 将未配置 `push.default` 与显式 `push.default=simple` 拆为两条独立 AC（`7/8`）；展开 `BR-20`/`BR-21` 完整逐文件 `Docs and compatibility impact`（含 `BR-20` 补入 `docs/development/commands/for-each-ref.md`）；清理 `BR-06`/`07`/`25`/`29`/`27`/`26`/`28` 重复文档碎片并统一 `split-from` 谱系；修正 R19 日志 `P3` → `P2`；全量同步至 31 卡 | `BR-06`→`06,31`；`BR-27`→`27,30` | 全文 |
| 2026-11-17 05:22:00 UTC | Claude & Codex R21 FAIL | 将 `BR-06` 拆出 `BR-32`（`@{u}` 与大小写变体 `@{UPSTREAM}`/`@{U}`，`6/8`），使 `BR-06`（`5/8`）与 `BR-32` 每条 AC 严格单谓词；全部 32 张卡补齐同卡 ER-06/ER-06a 文档逐文件交付验收门（不计入 G-03 卡特有 AC 分子）；将 `DEP-BR-08` 覆盖范围、实施顺序、`发布窗口顺序`、审计表、追溯表（补齐 `BR-31`/`BR-32`/`BR-30` 并修正 `BR-27` 测试名 `push_simple_and_tracking`）、`评审结论`、`M2`/`M3` 与 `#684` 部分编号（`1/9`..`9/9`）全量同步至 32 卡；校准 `BR-01`/`14`/`07`/`10` AC 计数、清理 `BR-28` 重复文档碎片、对齐 `BR-25`/`29`/`27`/`26`/`28` 过渡文档清除说明及 `status.rs:883` 锚点 | `BR-06`→`06,32,31` | 全文 |
| 2026-11-17 05:26:00 UTC | Codex R22 FAIL | 将 `BR-32` AC6（`branch -f @{UPSTREAM}` 与 `@{U}`）拆为两条独立单谓词 AC（`7/8`）；将 `BR-31` detached HEAD 隐式 `@{upstream}`/`@{u}` 失败与显式 `<branch>@{upstream}`/`<branch>@{u}` 成功拆为 4 条独立单谓词 AC（共 `5/8`），同步更新 `Granularity` 与审计表 | N/A | 审计表、`BR-32`、`BR-31`、实施证据汇总、review log |
| 2026-11-17 05:30:00 UTC | Claude R22 FAIL | 补齐跨计划共享源码写集依赖 `DEP-BR-09`（`util.rs`）、`DEP-BR-10`（`fetch.rs`）、`DEP-BR-11`（`branch.rs`）、`DEP-BR-12`（`internal/branch.rs`）并接入全部对应任务卡与审计表、更新 `与其它计划的关系`（含 10 张 `util.rs` 卡及 `diff`）；将网站文档路径统一改为本地已挂载的 `/Volumes/Sea/libra-backend`（`cf` 分支）；拆细 `BR-29` AC4（`-f` 与 `--set-upstream-to`，`5/8`）、补齐 `BR-21` `%(refname)` AC（`3/8`）、移除 `BR-30` 重复过渡说明并修正 `EX-BR-01` 评审轮次为 `R14` | N/A | 全文 |
| 2026-11-17 05:35:00 UTC | Codex R24 FAIL（Claude R24 `ECONNRESET`） | 将 `BR-21` 拆细为独立单谓词 AC；在 `DEP-BR-02` 明确本地 `/Volumes/Sea/libra-backend`（`cf` 分支）与执行机挂载授权路径 `/Volumes/Sea/libra-backend` 的优先核对规则（无有效 `cf` 仓库则 STOP） | N/A | `DEP-BR-02`、审计表、`BR-21`、实施证据汇总、review log |
| 2026-11-17 05:42:00 UTC | Codex R25 FAIL（Claude R25 `ECONNRESET`） | 将 `BR-21` AC1/AC2 进一步拆细为 8 条严格单谓词 AC（分别独立验证 detached-at 的 `%(HEAD)=*`/`%(refname)`/`%(refname:short)`、detached-from 的 `%(refname)`/`%(refname:short)`、列表顺序、非 detached 与 `-r` 排除，同步 `AC=8/8` 至 `Granularity` 与审计表）；移除 `Codex review log` 中并无独立评审记录的 `R23` 占位行（R23 修订已并入 R24 统一复审） | N/A | 审计表、`BR-21`、修订历史、实施证据汇总、review log |
| 2026-11-17 07:46:33 UTC | Codex R26 FAIL（Claude R26 `ECONNRESET`） | 网站写集与 `DEP-BR-02` 固定为 `/Volumes/Sea/libra-backend`（缺失则 STOP）；`BR-27`/`BR-30`/`BR-26` 补入 `branch` 中英文、开发文档与网站 `branch.en.md` | N/A | `DEP-BR-02`、`BR-26`/`27`/`30`、Review log |
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
| 2026-11-17 12:27:28 UTC | Codex R46 FAIL | M0 改为 Codex 单审；新增 `BR-38` 承接普通/unborn/detached/column 的默认样式与自定义槽矩阵，避免把路径合并谓词塞入 `BR-03`/`BR-37`；按 Git 默认样式修正普通本地非当前分支无 ANSI 的断言，并同步 36 卡 DAG/粒度/追溯 | `BR-37` → `BR-37,BR-38` | 头部、成功定义、DAG、`DEP-BR-08`、Phase 0/2、审计表、`BR-03`/`BR-37`/`BR-38`/`BR-04`、追溯表、M0/M1/M3、Review log |
| 2026-11-17 12:27:28 UTC | Codex R47 FAIL | 新增 `REL-BR-03` 与发布点 `BR-39`，将 `BR-03`/`BR-37`/`BR-38` 改为家族子卡，确保颜色模式、槽和四类入口首次发布前共同验收；同步实施证据最新修订时间 | `BR-38` → `BR-38,BR-39` | 发布分组、DAG、默认覆盖、审计表、`BR-03`/`BR-37`/`BR-38`/`BR-39`/`BR-04`、追溯表、M1/M3、实施证据、Review log |
| 2026-11-17 12:27:28 UTC | Codex R48 FAIL/P2 | `ADR-BR-02` 与完成判据改为独立卡逐卡发布、`REL-BR-03` 子卡继承 `BR-39` C/D；补齐家族窗口禁止插入项及 Libra/网站发布前逆序恢复和发布后前滚规则 | N/A | `ADR-BR-02`、发布分组、完成判据、Review log |
| 2026-11-17 12:27:28 UTC | Codex R49-A/B FAIL | 全局发布条款改为仅独立卡与 `BR-39` 自行 C/D；`DEP-BR-08` 改家族级锁贯穿四卡并在统一推送或回滚后释放；发布禁令仅限子卡阶段，允许 `BR-39` 聚合后统一发布；跨计划顺序统一为 `BR-10`/`BR-22` → `WC-10` → `PM-21` | N/A | 执行检查、`DEP-BR-08`、`DEP-BR-12`、发布分组、Review log |
| 2026-11-17 12:27:28 UTC | Codex R50-A/B FAIL/P2 | `BR-03` 增加 `--no-color` 覆盖与共享 OutputConfig/非 branch 回归；`BR-25` 增加 upstream 跨 remote 拒绝；`BR-22` 增加中途失败回滚及 AC/VER；性能数值改为非门禁观测目标 | N/A | `BR-03`/`BR-22`/`BR-25`、审计表、追溯表、性能摘要、Review log |
| 2026-11-17 12:27:28 UTC | Codex R51-A/B FAIL/P2 | `BR-09` 增加 `-M` 覆盖目标失败恢复，`BR-11` 增加 copy 记录断言；将 7 项过渡守卫拆至 `BR-40` 并置于 `BR-25` 前，`BR-25` 仅保留同 remote 成功、跨 remote 失败与 remote 选择，取消 `EX-BR-01` | `BR-25` → `BR-40,BR-25` | 头部、成功定义、DAG、默认覆盖、审计表、`BR-40`/`BR-25`、追溯表、M3、waiver、Review log |
| 2026-11-17 12:27:28 UTC | Codex R52-A/B FAIL/P2 | `DEP-BR-09` 纳入 `BR-40`；`BR-09` 交付 `-M` 成功路径及成功/失败双测试；`BR-40` 明确交付最小 `@{push}` 入口与 7 个可区分过渡拒绝；文档清除责任改指 `BR-40` | N/A | `DEP-BR-06`、`DEP-BR-09`、`BR-09`、`BR-40`、`BR-27`/`BR-26` 文档、审计表、追溯表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R53-A/B FAIL/P2 | 普通 `-m` 恢复范围补入源/目标 `branch.<name>.*` config 快照与逐项断言；风险表纳入 `BR-40`，`BR-25` 明确扩展现有入口 | N/A | `BR-09`、`BR-25`、风险表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R54-A/B FAIL | 普通 `-m` 成功路径补全量配置迁移与清理；`BR-40` 增加 `push.default=upstream` 过渡拒绝，`BR-25` 同卡移除该限制 | N/A | `BR-09`、`BR-40`/`BR-25`、审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R55-A/B 评审 | R55-B 分区 PASS，无 P0/P1/P2。R55-A P1：`-M` 成功配置状态不明确，已明确源配置迁移、覆盖前目标配置与旧名称配置清理及逐项断言 | N/A | `BR-09`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R56-A PASS | R56-A 覆盖第 1–1000 行、完成判据与 `DEP-PM-06`，普通 `-m`/`-M` 配置迁移与恢复、`DEP-BR-09`、`REL-BR-03` 均无 P0/P1/P2；与 R55-B 分区 PASS 共同构成计划 Codex 单审通过 | N/A | 头部、Review log |
| 2026-11-17 12:27:28 UTC | Codex R53-B P2 | 风险表关联卡纳入 `BR-40`；`BR-25` 描述明确在既有最小入口上扩展而非重新新增解析器 | N/A | `BR-25`、风险表、Review log |

## 已决议设计决策

### ADR-BR-01: 共享修订语法与格式原子下沉至公共引擎

- **Status:** Accepted
- **Context:** `#684`/`#685`（`@{upstream}`/`@{push}`/`@{-N}`）与 `#700`/`#701`（`%(ahead-behind:...)` / detached HEAD）属于全仓通用修订解析（`src/utils/util.rs`）与通用 ref 格式化引擎（`src/command/for_each_ref.rs`）。
- **Decision:** `BR-06`/`BR-07` 在 `src/utils/util.rs` 提供提交解析与分支名解释 helper 并接入 `src/command/branch.rs`；`BR-20`/`BR-21` 在 `src/command/for_each_ref.rs` 实现格式原子与 detached HEAD 渲染。
- **Alternatives considered:** 仅在 `branch.rs` 局部字符串替换。拒绝理由：违反 GC-02 单一事实源。
- **Consequences:** 触及共享 helper 命中 ER-13 `T-1`，对应卡执行 C 组时必须跑双环境完整 nextest。
- **Revisit when:** 若后续引入独立 revision parser crate 时再迁移。

### ADR-BR-02: `branch.rs` 单文件写集串行化推进

- **Status:** Accepted
- **Context:** 任务卡共享 `src/command/branch.rs` 与 `tests/command/branch_test.rs`。
- **Decision:** 按 G-10 禁止 I–I 并发，活动卡串行执行。除 `REL-BR-03` 外，每张活动卡独立 `patch + 1` 并触发 `release.yml`。`BR-03`/`BR-37`/`BR-38` 是 `REL-BR-03` 的家族子卡，只完成本地提交，继承 `BR-39` 的统一首次发布、C/D 与 Cloudflare 证据；`BR-39` 是本计划唯一家族发布点。`BR-35` 已撤回。
- **Alternatives considered:** 合并为少数大卡，或让三个颜色子卡各自独立发布。前者违反 G-01/G-03/G-04，后者会在首次完整验收前留下颜色模式、槽或渲染入口的中间态，故均采用 `REL-BR-03` 共同首发。
- **Consequences:** 无并发冲突；独立卡按 `immutable-release` 前滚，家族子卡只有在 `BR-39` 取得完整 C/D 后才能 `done/complete`。
- **Revisit when:** 仅当未来拆分 `branch.rs` 为子模块后可调整。

## 全局工程约束

继承 `plan-template.md` 全部 `GC-01`..`GC-13`。补充本计划专项约束：
- **GC-BR-01:** 凡修改用户可见命令行为的卡，必须在同一卡更新对应命令 `docs/commands/<cmd>.md`（逐文件同步 EN 与 zh-CN）、`docs/development/commands/<cmd>.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`（按 ER-06a 核对相邻仓库 `/Volumes/Sea/libra-backend` 的 VCS 与 `cf` 分支，无有效 `cf` 仓库则 STOP 报告）及 `COMPATIBILITY.md`。
- **GC-BR-02:** 新增或修正错误码/退出码时，同步核对 `docs/error-codes.md`；不引入未解释的 `unwrap()`/`expect()`/`panic!()`（GC-11）。

## 执行检查必备需求（强制）

继承 `plan-template.md` 全部 `ER-01`..`ER-14`。
- 每张卡开工前执行 `ER-01`、`ER-02`（刷新行号锚点）与 `ER-03`（复核 `G-*`）。
- 每张独立活动卡及 `BR-39` 家族发布点完成后按 `ER-04`、`ER-05`、`ER-07`、`ER-08`（`patch + 1`、三处版本面同步、工具链刷新 `Cargo.lock`、`gh release create`）与 `ER-13`/`ER-14` 完成 C/D 组；`REL-BR-03` 子卡仅完成本地 A/B 与网站本地门，继承 `BR-39` 的 C/D。`BR-33` 与 `BR-35` 已撤回，不发布。
- 全部 38 张活动卡完成后执行计划收口全量门：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

## 实施顺序

按 G-10 串行化写集，形成与 Phase 0/1/2 一致的无环顺序链：
- `BR-01 -> BR-02 -> BR-03 -> BR-37 -> BR-38 -> BR-39 -> BR-04 -> BR-05 -> BR-06 -> BR-32 -> BR-31 -> BR-07 -> BR-08 -> BR-09 -> BR-10 -> BR-34 -> BR-36 -> BR-11 -> BR-12 -> BR-13 -> BR-14 -> BR-15 -> BR-16 -> BR-17 -> BR-18 -> BR-19 -> BR-20 -> BR-21 -> BR-22 -> BR-23 -> BR-24 -> BR-40 -> BR-25 -> BR-29 -> BR-27 -> BR-30 -> BR-26 -> BR-28`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-BR-01 | incoming | 上游基线 | `main@62abb622f0eb2626934633ada78061963ebf1f44` | 仓库维护者 | 干净可编译基线且 `compat_version_surface_sync` 通过 | `Cargo.toml:3` | 若上游版本面不一致，先建修复卡对齐再开工 |
| DEP-BR-02 | incoming | 相邻文档仓库 | `/Volumes/Sea/libra-backend`（唯一网站交付仓库，`cf` 分支；不接受其它检出替代） | 文档发布者 | 该路径存在、VCS 检查通过且当前分支为 `cf` | `ER-06a` 检查命令 | 路径不存在、不是唯一 VCS 元数据、或不在 `cf` 时 STOP 并报告，禁止改写其它检出 |
| DEP-BR-03 | incoming | 外部前置 Issue | `libra#686`（全局 `-C` 标志） | CLI 维护者 | `libra -C <dir> branch ...` 正常执行 | `gh issue view 696` | 仅影响全套 upstream `t3200#179` 运行；不影响 `BR-17` 本身单测/集成验收 |
| DEP-BR-04 | incoming | 外部前置 Issue | `libra#676`（`checkout --orphan` 级联前置） | CLI 维护者 | 上游完整 `t3204` 运行前置就绪 | Issue #680/#685 关联项 | 仅影响完整上游脚本全跑；本卡聚焦测试不阻塞 |
| DEP-BR-05 | incoming | 外部前置 Issue | `libra#541`（`update-ref` 仅限 `refs/heads`） | CLI 维护者 | 通用 ref 更新支持非 `refs/heads` 引用 | Issue #680/#689 关联项 | 仅影响上游脚本 setup；不影响 `BR-03` 本地测试 |
| DEP-BR-06 | incoming | 外部前置 Issue | `libra#675`（`branch -M` 强制重命名） | CLI 维护者 | `branch -M` 可用于上游测试 setup | Issue #688 关联项 | 仅影响完整上游脚本 setup；`BR-09` 本卡必须交付 `-M` 覆盖目标的成功路径，不依赖 `#675` 才允许本地验收 |
| DEP-BR-07 | incoming | 外部前置 Issue | `libra#556`（symref/detached-from 底层前置） | CLI 维护者 | 上游完整 `t3203` symref 用例前置就绪 | Issue #701 关联项 | 仅影响完整上游脚本跑通；`BR-21` 自身聚焦测试不阻塞 |
| DEP-BR-08 | incoming | 跨计划共享文档写集 | `COMPATIBILITY.md`、`docs/error-codes.md`（与并行计划 `grep`/`checkout-switch-restore`/`diff` 共享） | 仓库协调者 | 覆盖全部活动卡（`BR-01`..`BR-32`、`BR-34`、`BR-36`..`BR-40`；`BR-33` 与 `BR-35` 不写文件）的串行编辑锁：独立卡逐卡申请、该卡推送后释放；`REL-BR-03` 由同一交付者从 `BR-03` 开工起连续持有家族级锁，贯穿 `BR-03`/`BR-37`/`BR-38`/`BR-39`，在 `BR-39` 统一推送后释放；发布前逆序恢复全部完成且确认无家族提交推送后也释放。`COMPATIBILITY.md` 仅原地改 `branch`/`for-each-ref`/`fetch`/`rev-parse` 专属行，`docs/error-codes.md` 允许 `BR-05`/`BR-08` 追加错码行 | 本计划依赖登记与协调者逐卡/家族放行记录（合入后同步 `plan-status.md`） | 未获当卡/家族串行编辑锁前阻塞开工编辑对应共享文件（不仅限推送），禁止并行计划同时修改同一文件（G-10） |
| DEP-BR-09 | incoming | 跨计划共享源码写集 | `src/utils/util.rs`（与 `clone`/`describe`/`merge-pull`/`global-init-infra` 共享） | 仓库协调者 | `BR-06`/`32`/`31`/`07`/`40`/`25`/`29`/`27`/`30`/`26`/`28` 修改 `util.rs` 前取得协调者串行放行，合入后立即释放 | 协调者串行调度记录 | 未获放行前阻塞编辑 `src/utils/util.rs`（G-10） |
| DEP-BR-10 | incoming | 跨计划共享源码写集 | `src/command/fetch.rs`（与 `fetch-push`/`clone` 共享） | 仓库协调者 | `BR-24` 修改 `fetch.rs` 前取得协调者串行放行，合入后立即释放 | 协调者串行调度记录 | 未获放行前阻塞编辑 `src/command/fetch.rs`（G-10） |
| DEP-BR-11 | incoming | 跨计划共享源码写集 | `src/command/branch.rs`（与 `checkout-switch-restore#CSR-10` 共享） | 仓库协调者 | 修改 `branch.rs` 的卡片取得协调者串行放行并与 `CSR-10` 错开窗口 | 协调者串行调度记录 | 禁止与 `CSR-10` 并发修改 `src/command/branch.rs`（G-10） |
| DEP-BR-12 | incoming/outbound | 跨计划共享源码写集 | `src/internal/branch.rs`（与 `worktree#WC-10`/`plumbing-misc#PM-21` 共享；`WC-10` 依赖 `BR-10`/`BR-22`） | 仓库协调者 | `BR-10`/`BR-34`/`BR-36`/`BR-22` 修改 `internal/branch.rs` 前取得放行，`PM-21` 错开窗口；顺序统一为 `BR-10`/`BR-22` → `WC-10` → `PM-21`，禁止反向等待形成环路 | 协调者串行调度记录 | 禁止并发修改 `src/internal/branch.rs`（G-10） |
| DEP-BR-13 | incoming | 跨计划共享源码写集 | `src/cli.rs` 与 `src/utils/output.rs`（同批多计划共享） | 仓库协调者 | `BR-03` 为传递显式 `--color` 状态修改上述文件前取得唯一串行窗口，合入后立即释放 | 协调者串行调度记录 | 未获放行前阻塞编辑 `src/cli.rs` 与 `src/utils/output.rs`（G-10） |

### 发布分组与并发窗口

默认每张卡独立发布切片（G-07：`Release boundary = independent`，`Version increment = patch`）。`REL-BR-03` 是唯一家族例外：`BR-03`（颜色模式）→ `BR-37`（自定义槽）→ `BR-38`（四类渲染路径）均为 `family child`，由 `BR-39` 作为唯一 `family release point` 共同发布，禁止在 `BR-39` 前单独推送 Libra `main` 或发布 tag。三张实现卡各自完成 focused 验收、fmt/严格 Clippy、网站本地门和本地签名提交；`BR-39` 跑全部颜色聚合测试、双环境 `T-2` 全量、版本面同步、构建、统一推送与 `gh release create`，子卡继承其 C/D。窗口内的子卡阶段禁止插入任何其它任务卡、版本 bump、Libra `main` 推送、tag/release 或网站 `cf` 推送；只有 `BR-39` 在全部聚合门通过后才可执行本卡指定的版本 bump、统一 Libra 推送、tag/release 与网站 `cf` 推送。发布前失败时先保留现场与诊断，按 `BR-39`（未提交则不创建）→ `BR-38` → `BR-37` → `BR-03` 逆序核对并 `libra revert` 对应本地提交；网站按 `BR-39`（若有）→ `BR-38` → `BR-37` → `BR-03` 逆序 `libra revert` 本地 `cf` 提交，且不得触碰无关改动。任一提交已推送或 tag 已创建后不再逆序回退，改按 `immutable-release` 前滚 `patch + 1` 并给降级指引。创建、重命名、复制仍各自独立发布；`BR-35` 已撤回。失败时按 `immutable-release` 前滚。

- **并发声明:** 全串行（按 G-10 禁止 I–I 并发）。
- **发布者:** 单一串行发布者。
- **发布窗口顺序:** `BR-01` → `BR-02` → `REL-BR-03`（`BR-03` → `BR-37` → `BR-38` → `BR-39`） → `BR-04` → `BR-05` → `BR-06` → `BR-32` → `BR-31` → `BR-07` → `BR-08` → `BR-09` → `BR-10` → `BR-34` → `BR-36` → `BR-11` → `BR-12` → `BR-13` → `BR-14` → `BR-15` → `BR-16` → `BR-17` → `BR-18` → `BR-19` → `BR-20` → `BR-21` → `BR-22` → `BR-23` → `BR-24` → `BR-40` → `BR-25` → `BR-29` → `BR-27` → `BR-30` → `BR-26` → `BR-28`，同一时刻仅允许 1 张卡/家族处于已 bump 未完成推送/发布状态（ER-12）。

### Phase 0: 参数校验、查询与上游基础修复（BR-01 .. BR-05）

**目标:** 修复 `--column -v` 互斥、`--list <pattern>`、`color.branch.*`、`-v`/`-vv` 名称列对齐、`--set-upstream-to refs/heads/` 与 `--unset-upstream` 无上游报错。

**进入条件:**

- 本计划通过 Codex review（`VERDICT: PASS`），且 `DEP-BR-01` 基线 parity 预检通过。

**退出条件:**

- `BR-01`..`BR-05`、`BR-37`、`BR-38` 与 `BR-39` 全部达到 `Lifecycle=done`、`Acceptance=complete`。`BR-33` 已撤回，不是本阶段完成门。

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

- `BR-01`..`BR-32`、`BR-34`、`BR-36`..`BR-40` 全部 `done/complete`，收口全量门全绿。`BR-33` 与 `BR-35` 已撤回，不进入本退出条件。

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
| `BR-05`, `BR-06`, `BR-32`, `BR-31`, `BR-07`, `BR-08`, `BR-09`, `BR-10`, `BR-34`, `BR-36`, `BR-11`, `BR-20`, `BR-21`, `BR-22`, `BR-40`, `BR-25`, `BR-29`, `BR-27`, `BR-30`, `BR-26`, `BR-28` | `Full-suite trigger` | `T-1`：修改跨切面共享 helper（`util.rs` / `for_each_ref.rs` / `internal/branch.rs` / `internal/reflog.rs`）或 `docs/error-codes.md` |
| `BR-03`, `BR-37`, `BR-38` | `Release boundary`, `Version increment`, `Release write set`, `Full-suite trigger`, `C/D coverage from` | 均为 `family child of REL-BR-03`；`Version increment=N/A`、`Release write set=N/A`、`Full-suite trigger=N/A`、`C/D coverage from=BR-39`。子卡只完成 focused/A/B、网站本地门和本地签名提交，不在 `BR-39` 前推送 Libra `main` |

**D 组远端后置门默认（适用于全部 `independent` 卡与 `BR-39` 家族发布点，ER-04）：**
- `base.yml`（PR 与 `push` `main`）：`format`、`clippy`、`redundancy`、`test` 同一 head SHA 全绿（以现场 `.github/workflows/base.yml` 实际 job 为准）。
- `codeql.yml`（PR 与 `push` `main`）：CodeQL 安全扫描全绿。
- `release.yml`（`push` `v<version>` tag，由 `gh release create` 触发）：多平台编译、Cloudflare R2 上传与 Homebrew tap 更新全绿。

**规则 waiver（`EX-*`）：** 无（原 `EX-BR-01` 已取消：7 项过渡守卫改由 `BR-40` 在 `BR-25` 前独立交付，无需超限豁免）。

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| BR-01 | implementation | `--column` 与 `-v` 互斥校验 | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | DEP-BR-01,DEP-BR-08,DEP-BR-11 | 串行起点 | independent | N/A | N/A |
| BR-02 | implementation | `--list` 位置模式过滤 | patch+1前滚修复 | yes | yes | 3/8 | 2/8 | 1/1 | M | BR-01,DEP-BR-08,DEP-BR-11 | 序列化于 BR-01 | independent | N/A | N/A |
| BR-03 | implementation | color.branch 模式含完整 auto | patch+1前滚修复 | yes | yes | 8/8 | 8/8 | 2/3 | M | BR-02,DEP-BR-08,DEP-BR-11,DEP-BR-13 | 序列化于 BR-02 | family child | N/A | N/A |
| BR-33 | superseded | 基线已对齐，不得开工 | N/A | N/A | N/A | N/A | N/A | N/A | N/A | N/A | 不写文件 | none | BR-03 | N/A |
| BR-37 | implementation | color.branch 五个槽自定义样式 | patch+1前滚修复 | yes | yes | 8/8 | 8/8 | 1/1 | M | BR-03,DEP-BR-08,DEP-BR-11 | 序列化于 BR-03 | family child | BR-03 | N/A |
| BR-38 | implementation | 四类列表默认与自定义颜色渲染矩阵 | patch+1前滚修复 | yes | yes | 4/8 | 4/8 | 1/1 | M | BR-37,DEP-BR-08,DEP-BR-11 | 序列化于 BR-37 | family child | BR-37 | N/A |
| BR-39 | release | REL-BR-03 颜色行为家族发布点 | immutable-release | yes | yes | 4/12 | 2/12 | 0/0 | S | BR-38,DEP-BR-08,DEP-BR-11 | 序列化于 BR-38 | family release point | N/A | N/A |
| BR-04 | implementation | `--set-upstream-to refs/heads/` | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | BR-39,DEP-BR-08,DEP-BR-11 | 序列化于 BR-39 | independent | N/A | N/A |
| BR-05 | implementation | `--unset-upstream` 无上游报错 | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | BR-04,DEP-BR-08,DEP-BR-11 | 序列化于 BR-04 | independent | N/A | N/A |
| BR-06 | implementation | `@{upstream}` 修订与分支名解释 | patch+1前滚修复 | yes | yes | 5/8 | 2/8 | 2/2 | M | BR-05,DEP-BR-08,DEP-BR-09,DEP-BR-11 | 序列化于 BR-05 | independent | N/A | N/A |
| BR-32 | implementation | `@{u}` 与大小写变体 `@{UPSTREAM}`/`@{U}` | patch+1前滚修复 | yes | yes | 7/8 | 2/8 | 2/2 | M | BR-06,DEP-BR-08,DEP-BR-09,DEP-BR-11 | 序列化于 BR-06 | independent | BR-06 | N/A |
| BR-31 | implementation | `@{upstream}` detached HEAD 规则 | patch+1前滚修复 | yes | yes | 5/8 | 2/8 | 2/2 | M | BR-32,DEP-BR-08,DEP-BR-09,DEP-BR-11 | 序列化于 BR-32 | independent | BR-06 | N/A |
| BR-07 | implementation | `@{-N}` 与 `@{-1}@{upstream}` 解释 | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 2/2 | M | BR-31,DEP-BR-08,DEP-BR-09,DEP-BR-11 | 序列化于 BR-31 | independent | N/A | N/A |
| BR-08 | implementation | `-m` 缺参退出码 128 与提示 | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | BR-07,DEP-BR-08,DEP-BR-11 | 序列化于 BR-07 | independent | N/A | N/A |
| BR-09 | implementation | `-m` 重命名迁移 reflog | patch+1前滚修复 | yes | yes | 8/8 | 5/8 | 2/2 | M | BR-08,DEP-BR-08,DEP-BR-11 | 序列化于 BR-08 | independent | N/A | N/A |
| BR-10 | implementation | 创建分支 D/F 冲突检查与错误点名 | patch+1前滚修复 | yes | yes | 4/8 | 2/8 | 2/2 | M | BR-09,DEP-BR-08,DEP-BR-11,DEP-BR-12 | 序列化于 BR-09 | independent | N/A | N/A |
| BR-34 | implementation | 重命名 D/F 冲突检查、错误点名与源排除 | patch+1前滚修复 | yes | yes | 5/8 | 1/8 | 2/2 | M | BR-10,DEP-BR-08,DEP-BR-11,DEP-BR-12 | 序列化于 BR-10 | independent | BR-10 | N/A |
| BR-35 | withdrawn | 已撤回，无写集 | N/A | N/A | N/A | N/A | N/A | 0/0 | N/A | N/A | 无 | withdrawn | N/A | N/A |
| BR-36 | implementation | 复制 D/F 冲突检查与错误点名 | patch+1前滚修复 | yes | yes | 8/8 | 1/8 | 2/2 | M | BR-34,DEP-BR-08,DEP-BR-11,DEP-BR-12 | 序列化于 BR-34 | independent | BR-34 | N/A |
| BR-11 | implementation | `--copy` 复制 reflog 与全量配置 | patch+1前滚修复 | yes | yes | 5/8 | 3/8 | 2/2 | M | BR-36,DEP-BR-08,DEP-BR-11 | 序列化于 BR-36 | independent | N/A | N/A |
| BR-12 | implementation | `-c` 同名复制 (`m2 m2`) 成功 | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | BR-11,DEP-BR-08,DEP-BR-11 | 序列化于 BR-11 | independent | N/A | N/A |
| BR-13 | implementation | `-c -f` 强制复制等价 `-C` | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | BR-12,DEP-BR-08,DEP-BR-11 | 序列化于 BR-12 | independent | N/A | N/A |
| BR-14 | implementation | `-i` 短选项别名绑定 `--ignore-case` | patch+1前滚修复 | yes | yes | 1/8 | 2/8 | 1/1 | M | BR-13,DEP-BR-08,DEP-BR-11 | 序列化于 BR-13 | independent | N/A | N/A |
| BR-15 | implementation | `--show-current` detached HEAD 静默 | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | BR-14,DEP-BR-08,DEP-BR-11 | 序列化于 BR-14 | independent | N/A | N/A |
| BR-16 | implementation | 重复 `--merged` / `--no-merged` 过滤 | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | BR-15,DEP-BR-08,DEP-BR-11 | 序列化于 BR-15 | independent | N/A | N/A |
| BR-17 | implementation | `--forked <pattern>` 分叉列表 | patch+1前滚修复 | yes | yes | 4/8 | 2/8 | 1/1 | M | BR-16,DEP-BR-08,DEP-BR-11 | 序列化于 BR-16 | independent | N/A | N/A |
| BR-18 | implementation | `--sort=objecttype` 排序键 | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | BR-17,DEP-BR-08,DEP-BR-11 | 序列化于 BR-17 | independent | N/A | N/A |
| BR-19 | implementation | 重复 `--sort` 多键排序 | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | BR-18,DEP-BR-08,DEP-BR-11 | 序列化于 BR-18 | independent | N/A | N/A |
| BR-20 | implementation | `%(ahead-behind:...)` 格式原子 | patch+1前滚修复 | yes | yes | 3/8 | 2/8 | 1/2 | M | BR-19,DEP-BR-08,DEP-BR-11 | 序列化于 BR-19 | independent | N/A | N/A |
| BR-21 | implementation | `--format` 包含 detached HEAD 伪条目 | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 1/2 | M | BR-20,DEP-BR-08,DEP-BR-11 | 序列化于 BR-20 | independent | N/A | N/A |
| BR-22 | implementation | `branch -r -d`/`-D` 删除远程追踪分支 | patch+1前滚修复 | yes | yes | 4/8 | 3/8 | 2/2 | M | BR-21,DEP-BR-08,DEP-BR-11,DEP-BR-12 | 序列化于 BR-21 | independent | N/A | N/A |
| BR-23 | implementation | `--sort=[-]ahead-behind:<rev>` 排序键 | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | BR-22,DEP-BR-08,DEP-BR-11 | 序列化于 BR-22 | independent | BR-20 | N/A |
| BR-24 | implementation | `fetch --all` 尊重 `skipFetchAll` | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | BR-23,DEP-BR-08,DEP-BR-10 | 序列化于 BR-23 | independent | BR-22 | N/A |
| BR-40 | implementation | `@{push}` 过渡 fail-closed 守卫 | patch+1前滚修复 | yes | yes | 8/8 | 1/8 | 1/1 | M | BR-24,DEP-BR-08,DEP-BR-09 | 序列化于 BR-24 | independent | BR-25 | N/A |
| BR-25 | implementation | `@{push}` remote 选择与 `upstream` | patch+1前滚修复 | yes | yes | 3/8 | 1/8 | 1/1 | M | BR-40,DEP-BR-08,DEP-BR-09 | 序列化于 BR-40 | independent | BR-06 | N/A |
| BR-29 | implementation | `@{push}` 显式分支/大小写与分支名解释 | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 2/2 | M | BR-25,DEP-BR-08,DEP-BR-09,DEP-BR-11 | 序列化于 BR-25 | independent | BR-25 | N/A |
| BR-27 | implementation | `@{push}` `simple` 与 `tracking_for_push_dest` | patch+1前滚修复 | yes | yes | 7/8 | 2/8 | 1/1 | M | BR-29,DEP-BR-08,DEP-BR-09 | 序列化于 BR-29 | independent | BR-25 | N/A |
| BR-30 | implementation | `@{push}` detached HEAD 规则 | patch+1前滚修复 | yes | yes | 2/8 | 2/8 | 1/1 | M | BR-27,DEP-BR-08,DEP-BR-09 | 序列化于 BR-27 | independent | BR-27 | N/A |
| BR-26 | implementation | `nothing`/`matching`/`current`/refspec/`mirror` | patch+1前滚修复 | yes | yes | 6/8 | 2/8 | 1/1 | M | BR-30,DEP-BR-08,DEP-BR-09 | 序列化于 BR-30 | independent | BR-25 | N/A |
| BR-28 | implementation | `for-each-ref %(push)` 复用与错误透传 | patch+1前滚修复 | yes | yes | 3/8 | 2/8 | 2/2 | M | BR-26,DEP-BR-08,DEP-BR-09 | 序列化于 BR-26 | independent | BR-26 | N/A |

---

### Task BR-01: 修复 `libra branch` 中 `--column` 与 `-v` 选项互斥校验（#678）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #678：当用户同时传入 `--column`（非 `--no-column` / `--column=never`）与 `-v`/`--verbose` 时，拒绝该选项冲突并输出 `options '--column' and '--verbose' cannot be used together`（exit 128，对齐 Git `die`）。
**Out of scope:** `--format` 与 `--column` 的交互（已由既有逻辑覆盖）；`color.branch.*` 配置（由 `BR-03` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `BranchArgs.column` 与 `verbose` 未在 clap 或 `execute_safe` 中做互斥校验 | `src/command/branch.rs:491-508,875-886` |
| `render_branch_output` 仅以 `verbose == 0` 静默禁用列布局 | `src/command/branch.rs:2913-2916` |
**Acceptance criteria:**
- [ ] `libra branch --column -v` 与 `libra branch --column=always -v` 失败（exit 128）且 stderr 包含 `options '--column' and '--verbose' cannot be used together`。
- [ ] `libra branch --column --no-column -v` 与 `libra branch --column=never -v`（最后一个选项禁用列布局）仍可正常执行。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::column_verbose_conflict`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** DEP-BR-01, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`（按 ER-06a 核对相邻仓库 `/Volumes/Sea/libra-backend` 的 VCS 与 `cf` 分支，无有效 `cf` 仓库则 STOP 报告）及 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--column与-v互斥校验; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DEP-BR-01,DEP-BR-08,DEP-BR-11; writeset=串行起点; release=independent; split-from=N/A; exception=N/A`

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
- [ ] `libra branch --list <pattern>...`（含单个及多个通配符模式，如 `feat/*`、`main`）进入 `BranchMode::List` 并按模式过滤输出。
- [ ] `libra branch -l <pattern>...` 与 `libra branch --list -r <pattern>...` / `-a <pattern>...` 均按 Git `wildmatch` 语义过滤。
- [ ] 未传 `--list` 时的 `libra branch <new_branch> [<start_point>]` 创建语义保持不变。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::list_pattern`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-01, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--list位置模式过滤; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-01,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-01; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-03: 支持 `color.branch.*` 配置与 `--color` 着色（#680）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #680 的颜色模式轴。本卡一次交付 `always`、`never` 和 `auto`：`auto` 在终端上着色，在管道或重定向里不着色。五个颜色槽的自定义样式拆出 `BR-37`。本卡使用 Git 默认槽颜色，并保留 `OutputConfig` 的 `--machine`、`--no-color`、`NO_COLOR` 与 `TERM=dumb` 安全契约。显式 `--color=auto` 必须与缺省 `auto` 区分传递。
**Out of scope:** `--format` 中的 `%(color:...)` 原子（已由 `for_each_ref` 处理）。`color.branch.current`、`local`、`remote`、`upstream`、`plain` 的自定义样式（拆出 `BR-37`）。基线 `list_name_column_width`（`branch.rs:3324-3339`）已在 `verbose >= 1` 时填充名称列（`branch.rs:2933-2937`）；本卡不得改动该填充。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `render_branch_output` 硬编码 `name.green()` / `name.red()`，未读取 `color.branch.*` 且未按 `output.color` 门控普通列表输出；`--column`、unborn 与 detached 使用独立渲染路径 | `src/command/branch.rs:2880-2967` |
| `OutputConfig` 已处理 machine/NO_COLOR/TERM=dumb，但 `color_raw=auto` 同时表示缺省与显式 `--color=auto` | `src/utils/output.rs:154-205`、`src/cli.rs:261-280,3530-3545` |
**Acceptance criteria:**
- [ ] 未传 `--color` 且生效值不是 `auto` 时，是否着色由 `color.branch` 相对 `color.ui` 的级联决定。
- [ ] 显式 `--color=always|never|auto` 覆盖 `color.branch`；CLI 原始 argv 区分缺省 `auto` 与显式 `--color=auto`，显式 `always` 不受 `NO_COLOR`/`TERM=dumb` 的自动抑制。
- [ ] 生效模式为 `auto` 且 stdout 是终端时启用分支着色逻辑；具体槽的默认样式及普通/unborn/detached/column 路径由 `BR-38` 验收，本卡不要求无颜色的 `plain` 槽输出 ANSI。
- [ ] 生效模式为 `auto` 且 stdout 被重定向时分支列表不含 ANSI 转义。
- [ ] `--color=never` 或全局 `--no-color` 时，即使 `color.branch=always` 也不含 ANSI 转义。
- [ ] `NO_COLOR` 仅在缺省/配置/显式 `auto` 下抑制颜色，不覆盖显式 `always`。
- [ ] `TERM=dumb` 仅在 `auto` 下抑制颜色，不覆盖显式 `always`。
- [ ] `--machine` 即使与显式 `--color=always` 同用也强制无 ANSI，并保持 machine 输出契约。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_mode_cascade`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_cli_override_including_explicit_auto`（new；覆盖显式 auto/always 与 NO_COLOR、TERM=dumb 优先级）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_auto_tty_enables_color`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_auto_redirect_no_ansi`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_never_and_no_color_flag_no_ansi`（new；分别覆盖 `--color=never` 与 `--no-color` 覆盖 `color.branch=always`）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_no_color_env_precedence`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_term_dumb_precedence`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_output_config_and_non_branch_regression`（new；覆盖 OutputConfig 缺省/显式 auto、`--no-color`、`--machine`，并至少验证一个非 branch 命令）

**Full-suite trigger:** N/A（`REL-BR-03` 家族子卡；双环境全量由 `BR-39` 的 `T-2` 执行）
**Dependencies:** BR-02, DEP-BR-08, DEP-BR-11, DEP-BR-13
**Deliverables:** N/A
**Implementation write set:** `src/cli.rs`, `src/utils/output.rs`, `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** `src/cli.rs`, `src/utils/output.rs`, `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** revert（`BR-39` 前）；发布后 immutable-release
**Estimated scope:** M（2 个落点，3 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-BR-03
**C/D coverage from:** BR-39
**Granularity:** `type=implementation; axis=color.branch 模式含完整 auto; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=2; prod-files=3; scope=M; deps=BR-02,DEP-BR-08,DEP-BR-11,DEP-BR-13; writeset=序列化于BR-02; release=family child; split-from=N/A; exception=N/A`

---

### Task BR-37: 自定义 `color.branch` 五个颜色槽（#680）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 自 `BR-03` 拆出（G-09）。本卡只交付五个颜色槽的自定义 ANSI 样式。`auto` 的终端与重定向行为已在 `BR-03` 一次交付，本卡不改模式。
**Out of scope:** `always`、`never`、`auto` 的模式判断（`BR-03`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| 格式化路径按终端状态决定 `auto` | `src/command/branch.rs:2880` |
**Acceptance criteria:**
- [ ] `color.branch.current` 自定义当前分支 ANSI 样式。
- [ ] `color.branch.local` 自定义本地分支 ANSI 样式。
- [ ] `color.branch.remote` 自定义远程分支 ANSI 样式。
- [ ] `color.branch.upstream` 自定义上游段 ANSI 样式。
- [ ] `color.branch.plain` 自定义普通段 ANSI 样式。
- [ ] `upstream`/`plain` 仅着色各自上游与普通文本段，不污染分支名槽。
- [ ] `never` 时四类列表均不泄漏自定义 ANSI 样式。
- [ ] `auto` 重定向时四类列表均不泄漏自定义 ANSI 样式。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_slot_current`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_slot_local`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_slot_remote`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_slot_upstream`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_slot_plain`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_slot_upstream_plain_segments`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_slot_never_no_ansi`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_slot_auto_redirect_no_ansi`（new）
**Full-suite trigger:** N/A（`REL-BR-03` 家族子卡；双环境全量由 `BR-39` 的 `T-2` 执行）
**Dependencies:** BR-03, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** revert（`BR-39` 前）；发布后 immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-BR-03
**C/D coverage from:** BR-39
**Granularity:** `type=implementation; axis=color.branch 五个槽自定义样式; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=1; scope=M; deps=BR-03,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-03; release=family child; split-from=BR-03; exception=N/A`

---

### Task BR-38: 对齐四类列表的默认与自定义颜色渲染（#680）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 自 `BR-37` 拆出（G-09），逐路径核对普通、unborn HEAD、detached HEAD 与 `--column` 列表的默认槽和自定义槽输出，防止只修普通列表而遗漏其它渲染入口。Git 默认 `plain` 本地非当前分支保持无 ANSI，不得为追求“全部有颜色”而偏离。
**Out of scope:** 颜色模式与全局禁色优先级（`BR-03`）；五个槽的配置解析（`BR-37`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| 普通、unborn、detached 与 `--column` 使用不同打印路径，当前 column 直接输出未着色名称 | `src/command/branch.rs:2880-2967` |
**Acceptance criteria:**
- [ ] 普通列表在默认与自定义槽配置下均符合 Git 预期：当前分支绿色、远程红色、本地非当前 `plain` 默认无 ANSI，且自定义槽只改变目标文本。
- [ ] unborn HEAD 在默认与自定义 `current` 槽下均按 Git 预期输出。
- [ ] detached HEAD 在默认与自定义 `current` 槽下均按 Git 预期输出。
- [ ] `--column` 在默认与自定义槽下均按 Git 预期输出当前/远程/本地分支，本地非当前 `plain` 默认无 ANSI。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_render_default_and_custom`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_unborn_default_and_custom`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_detached_default_and_custom`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::color_branch_column_default_and_custom`（new）
**Full-suite trigger:** N/A（`REL-BR-03` 家族子卡；双环境全量由 `BR-39` 的 `T-2` 执行）
**Dependencies:** BR-37, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** revert（`BR-39` 前）；发布后 immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-BR-03
**C/D coverage from:** BR-39
**Granularity:** `type=implementation; axis=四类列表默认与自定义颜色渲染矩阵; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=BR-37,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-37; release=family child; split-from=BR-37; exception=N/A`

---

### Task BR-39: 发布 `REL-BR-03` 颜色行为家族版本（#680）

**Task type:** release
**Lifecycle / Acceptance:** pending / 空
**Description:** 作为 `REL-BR-03` 唯一发布点，聚合 `BR-03`（模式与全局优先级）、`BR-37`（五个自定义槽）和 `BR-38`（普通/unborn/detached/column 渲染路径）的最终本地提交，在首次公开发布前共同验收完整颜色行为。
**Out of scope:** 子卡生产实现（分别由 `BR-03`/`BR-37`/`BR-38` 完成）；其它独立卡的发布。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| 任一子卡单独发布都会让部分渲染入口或槽配置处于未验收中间态 | `REL-BR-03`、`BR-03`、`BR-37`、`BR-38` 的写集与验收矩阵 |
**Acceptance criteria:**
- [ ] 三个家族子卡均已完成最终本地签名提交，且其提交未在 `BR-39` 前推送 Libra `main` 或触发 tag/release。
- [ ] `BR-39` 被测最终树包含三个子卡全部变更，并完成 `patch + 1`、三处版本面同步、工具链刷新 `Cargo.lock`、统一推送与 `gh release create`。
- [ ] `BR-39` 的颜色聚合验证覆盖 `BR-03` 8 项、`BR-37` 8 项与 `BR-38` 4 项卡特有验证，任何一项失败均不发布。
- [ ] `BR-39` 完成家族网站交付：三个子卡的本地 `cf` 提交均通过 sync/check、前端验证与网站 Claude Code PASS 后统一推送，并核验 Cloudflare 部署及生产正文。
- [ ] 同卡完成发布说明与兼容证据（ER-04/ER-08 强制门，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test color_branch_`（必须非零匹配并覆盖全部 20 项颜色测试）
- [ ] `source .env.test && cargo nextest run --test compat_version_surface_sync`
**Full-suite trigger:** `T-2`（`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`）
**Dependencies:** BR-38, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`
**Release write set:** Inherited
**Files likely touched:** `Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`
**Docs and compatibility impact:** 汇总 `REL-BR-03` 的发布说明、颜色行为兼容证据与家族网站交付记录；不改子卡已完成的文档写集。
**Rollback mode:** immutable-release
**Estimated scope:** S（仅发布面，无生产代码落点）
**Version increment:** patch
**Release boundary:** family release point of REL-BR-03
**C/D coverage from:** self（`BR-03`/`BR-37`/`BR-38` 继承本卡 C/D）
**Granularity:** `type=release; axis=REL-BR-03 颜色行为家族发布点; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/12; VER=2/12; landing=0; prod-files=0; scope=S; deps=BR-38,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-38; release=family release point; split-from=N/A; exception=N/A`

---

### Task BR-33: 已撤回（不得开工）

**Status:** superseded。基线 `src/command/branch.rs:3324-3339` 的 `list_name_column_width` 已按最长分支名计算列宽，`branch.rs:2933-2937` 已在 `-v`/`-vv` 使用该宽度。没有剩余实现缺口。本编号不再拥有写集、验收标准或发布边界，不计入 38 张活动卡。`BR-35` 同样已撤回。着色回归留在 `BR-03`，且该卡不得改动上述填充。

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
- [ ] `libra branch --set-upstream-to=refs/heads/<other>` 成功配置本地追踪（`branch.<cur>.remote = "."`、`branch.<cur>.merge = "refs/heads/<other>"`）。
- [ ] `libra branch --set-upstream-to=refs/heads/<self>` 与 `--set-upstream-to=<self>` 一致：发出 `warning: not setting branch '<self>' as its own upstream`，不写入自追踪配置。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::set_upstream_refs_heads`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests::set_upstream_impl_warns_on_self_upstream`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-39, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--set-upstream-to支持refs/heads/与自追踪警告; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-39,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-39; release=independent; split-from=N/A; exception=N/A`

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
- [ ] 目标分支无 `branch.<name>.remote` 且无 `branch.<name>.merge` 时，`--unset-upstream` 失败（exit 128）并提示 `branch '<name>' has no upstream information`。
- [ ] 目标分支存在上游配置时，`--unset-upstream` 正常清除配置并退出 0。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::unset_upstream_without_config`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 修改 `docs/error-codes.md`
**Dependencies:** BR-04, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `docs/error-codes.md`
**Release write set:** Inherited
**Files likely touched:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `docs/error-codes.md`
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md` 与 `docs/error-codes.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--unset-upstream无上游报错; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-04,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-04; release=independent; split-from=N/A; exception=N/A`

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
- [ ] `@{upstream}` 在修订解析中正确解析为当前分支上游追踪分支对应提交。
- [ ] `<branch>@{upstream}` 在修订解析中正确解析为 `<branch>` 上游追踪分支对应提交。
- [ ] `libra branch -f @{upstream} two`（本地上游）直接更新对应的本地分支而非报 `not a valid branch name`。
- [ ] 当 `@{upstream}` 指向远程追踪分支（如 `origin/x`）时，`libra branch -f @{upstream} <commit>` fail-closed 拒绝更新远程分支（`t3204#4`）。
- [ ] 无上游配置时执行 `@{upstream}` fail-closed 返回明确错误。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::rev_parse_test::upstream_revision`（new；含无上游报错断言）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::branch_upstream_name_interpretation`（new）
**Full-suite trigger:** T-1: 修改共享修订解析 helper `src/utils/util.rs`
**Dependencies:** BR-05, DEP-BR-08, DEP-BR-09, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `src/command/branch.rs`, `tests/command/rev_parse_test.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/revision.md`、`docs/commands/zh-CN/revision.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{upstream}核心修订与分支名解释; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-05,DEP-BR-08,DEP-BR-09,DEP-BR-11; writeset=序列化于BR-05; release=independent; split-from=N/A; exception=N/A`

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
- [ ] `@{u}` 在修订解析中正确解析为当前分支上游追踪分支对应提交。
- [ ] `<branch>@{u}` 在修订解析中正确解析为 `<branch>` 上游追踪分支对应提交。
- [ ] `@{UPSTREAM}` 大小写变体在修订解析中按 Git `object-name.c` 规则正常解析。
- [ ] `@{U}` 大小写变体在修订解析中按 Git `object-name.c` 规则正常解析。
- [ ] `libra branch -f @{u} two` 在 `branch` 分支名参数中解析为本地上游分支并更新之。
- [ ] `libra branch -f @{UPSTREAM} two` 在 `branch` 分支名参数中解析为本地上游分支并更新之。
- [ ] `libra branch -f @{U} two` 在 `branch` 分支名参数中解析为本地上游分支并更新之。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::rev_parse_test::upstream_short_and_case_variants`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::branch_upstream_short_and_case_name`（new）
**Full-suite trigger:** T-1: 修改共享修订解析 helper `src/utils/util.rs`
**Dependencies:** BR-06, DEP-BR-08, DEP-BR-09, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `src/command/branch.rs`, `tests/command/rev_parse_test.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/branch.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/revision.md`、`docs/commands/zh-CN/revision.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{u}与@{UPSTREAM}/@{U}大小写变体; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-06,DEP-BR-08,DEP-BR-09,DEP-BR-11; writeset=序列化于BR-06; release=independent; split-from=BR-06; exception=N/A`

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
- [ ] detached HEAD 下需要从 `HEAD` 推断分支的隐式 `@{upstream}` fail-closed 返回明确错误。
- [ ] detached HEAD 下需要从 `HEAD` 推断分支的隐式 `@{u}` fail-closed 返回明确错误。
- [ ] detached HEAD 下显式 `<branch>@{upstream}` 仍按 `<branch>` 配置正常解析。
- [ ] detached HEAD 下显式 `<branch>@{u}` 仍按 `<branch>` 配置正常解析。
- [ ] detached HEAD 下 `libra branch -f <branch>@{upstream} <commit>` 按 `<branch>` 上游分支名正常解释并更新目标本地分支。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::rev_parse_test::upstream_detached_head`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::branch_upstream_detached_explicit`（new）
**Full-suite trigger:** T-1: 修改共享修订解析 helper `src/utils/util.rs`
**Dependencies:** BR-32, DEP-BR-08, DEP-BR-09, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `src/command/branch.rs`, `tests/command/rev_parse_test.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/branch.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/revision.md`、`docs/commands/zh-CN/revision.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{upstream}detachedHEAD规则; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-32,DEP-BR-08,DEP-BR-09,DEP-BR-11; writeset=序列化于BR-32; release=independent; split-from=BR-06; exception=N/A`

---

### Task BR-07: 支持 `@{-N}` 与 `@{-1}@{upstream}` 修订与分支名解释（#685）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #685：在 `src/utils/util.rs` 的修订解析与分支名解释 helper 中支持 `@{-N}`（`N >= 1`）与链式 `@{-1}@{upstream}` / `@{-1}~1`，并接入 `src/command/branch.rs` 的创建/`-f`/`-u`/`--unset-upstream` 分支名参数路径。
**Out of scope:** `@{upstream}`/`@{u}` 基础解析（由 `BR-06`/`BR-31` 交付）；`@{push}`（由 `BR-25` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `resolve_revision_expression_typed` 拒绝 `-1` 等非纯数字索引，且 `branch` 的 `-u`/`--unset-upstream` 未通过分支名解释器解析 `@{-1}` | `src/utils/util.rs:2652-2656`、`src/command/branch.rs:1495-1687` |
**Acceptance criteria:**
- [ ] `@{-1}` 解析为上一次检出的分支名。
- [ ] `@{-N}` 解析为第 N 次先前检出对应的提交。
- [ ] `@{-1}@{upstream}` 在解析 `@{-1}` 后继续解析 `@{upstream}`。
- [ ] `@{-1}~1` 在解析 `@{-1}` 后继续应用 `~1`。
- [ ] `libra branch --set-upstream-to X @{-1}` 把上游设到解析出的分支。
- [ ] `libra branch --unset-upstream @{-1}@{upstream}` 移除 `branch.upstream-other.merge`（`#685` 文案里的 `branch.upstream-branch` 是笔误）。
- [ ] 历史不足 N 次时 `@{-N}` fail-closed。
- [ ] `@{-0}` fail-closed。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::rev_parse_test::previous_branch_nth_revision`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::branch_previous_branch_upstream`（new）
**Full-suite trigger:** T-1: 修改共享修订解析 helper `src/utils/util.rs`
**Dependencies:** BR-31, DEP-BR-08, DEP-BR-09, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `src/command/branch.rs`, `tests/command/rev_parse_test.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/revision.md`、`docs/commands/zh-CN/revision.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{-N}与链式@{upstream}修订与分支名解释; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-31,DEP-BR-08,DEP-BR-09,DEP-BR-11; writeset=序列化于BR-31; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-08: 修复 `libra branch -m` 无参数退出码为 128 并提示 `branch name required`（#687）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #687：将 `libra branch -m` 在无参数时的处理从 clap 的 129 参数缺失改为执行期 `branch name required` 报错并退出 128，对齐 Git。
**Out of scope:** `-m` 迁移 reflog（由 `BR-09` 承接）；`-c`/`-C` 缺参处理（仅在显式需要时另开卡）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `BranchArgs.rename` 声明 `num_args = 1..=2`，导致 `libra branch -m` 直接在 clap 解析层退出 129 | `src/command/branch.rs:429-430,2247-2255` |
**Acceptance criteria:**
- [ ] `libra branch -m`（0 个参数）退出码为 `128`，stderr 包含 `branch name required`。
- [ ] `libra branch -m <new>` 与 `libra branch -m <old> <new>` 保持既有重命名行为；超过 2 个参数仍返回用法错误（exit 129）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::rename_missing_arg_exit_128`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 修改 `docs/error-codes.md`
**Dependencies:** BR-07, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`, `docs/error-codes.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`COMPATIBILITY.md` 与 `docs/error-codes.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-m缺参退出码128与提示; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-07,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-07; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-09: 修复 `libra branch -m` 迁移源分支 reflog 至新分支（#688）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #688：`rename_branch_impl` 在重命名分支时将 `refs/heads/<old>` 的 reflog 完整迁移（并追加重命名记录）到 `refs/heads/<new>`，移除旧 reflog，使 `libra reflog exists <new>` 通过且 `<old>` 不再残留。
**Out of scope:** `branch --copy` 复制 reflog（由 `BR-11` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `rename_branch_impl` 仅更新 ref、HEAD 与 `MetadataKv`，未迁移 `src/internal/reflog.rs` 中的分支 reflog | `src/command/branch.rs:2282-2316` |
**Acceptance criteria:**
- [ ] `libra branch -m <old> <new>` 后，`libra reflog exists <new>` 返回 0。
- [ ] 新分支 reflog 含有原 `<old>` 的历史。
- [ ] 新分支 reflog 含有本次 rename 记录。
- [ ] 重命名后 `libra reflog exists <old>` 返回非零（旧 reflog 已清理）。
- [ ] 重命名中间步骤（新建目标 ref / 更新 HEAD / 迁移 metadata / 迁移 `branch.<name>.*` config / 迁移 reflog / 删除旧 ref）发生失败时，保持源分支与目标分支的 ref、HEAD、metadata、`branch.<name>.*` config 与 reflog 状态完整一致（事务回滚或显式补偿），不留半迁移残留。
- [ ] `libra branch -M <old> <existing-target>` 在中间步骤失败时，源分支、覆盖前目标分支及 HEAD 的 ref、metadata、config 与 reflog 均恢复至操作前状态。
- [ ] `libra branch -m <old> <new>` 成功时，除迁移 ref 与完整 reflog 外，还完整迁移 `branch.<old>.*`（含 `remote`、`merge` 及其他配置项）到 `branch.<new>.*`，并清理旧名称配置。
- [ ] `libra branch -M <old> <existing-target>` 成功时覆盖目标 ref 并迁移源分支完整 reflog 与 rename 记录；`branch.<old>.*` 全部配置完整迁移到 `branch.<new>.*`，覆盖前目标配置与旧名称配置均被清理，HEAD 指向覆盖后的正确状态。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::rename_moves_reflog`（new；同时断言 `remote`、`merge` 及其他 `branch.<old>.*` 配置完整迁移到 `<new>` 且旧配置被清理）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::rename_partial_failure_rollback`（new；故障注入验证中间失败时 ref/HEAD/metadata/config/reflog 状态一致，逐项比较源、目标操作前 `branch.<name>.*` 快照并断言恢复）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::rename_force_overwrite_target_rolls_back`（new；逐项比较覆盖前目标 ref/metadata/config/reflog 与源/HEAD 状态）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::rename_force_overwrite_target_succeeds`（new；先验证 `-M` 覆盖成功路径，逐项断言 `remote`、`merge` 及其他 `branch.<old>.*` 配置迁移、覆盖前目标配置和旧名称配置清理，再执行失败注入）
- [ ] `source .env.test && cargo nextest run --lib internal::reflog`
**Full-suite trigger:** T-1: 修改共享 reflog 存储模块 `src/internal/reflog.rs`
**Dependencies:** BR-08, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/internal/reflog.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-m重命名迁移reflog; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=5/8; landing=2; prod-files=2; scope=M; deps=BR-08,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-08; release=independent; split-from=N/A; exception=N/A`

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
- [ ] 当 `o/p` 存在时，`libra branch o` 退出非零。
- [ ] 上一条失败的 stderr 点名 `o/p`。
- [ ] 当 `o` 存在时，`libra branch o/p` 退出非零。
- [ ] 上一条失败的 stderr 点名 `o`。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::ref_namespace_df_conflict_create`（new）
- [ ] `source .env.test && cargo nextest run --lib internal::branch`
**Full-suite trigger:** T-1: 修改共享 helper `src/internal/branch.rs`
**Dependencies:** BR-09, DEP-BR-08, DEP-BR-11, DEP-BR-12
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/internal/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=创建分支D/F冲突检查与错误点名; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-09,DEP-BR-08,DEP-BR-11,DEP-BR-12; writeset=序列化于BR-09; release=independent; split-from=N/A; exception=N/A`

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
- [ ] 当 `o/p` 存在时，`libra branch -m o/o o` 退出非零。
- [ ] 上一条失败的 stderr 点名 `o/p`。
- [ ] 当 `o` 存在时，`libra branch -m x o/p` 退出非零。
- [ ] 上一条失败的 stderr 点名 `o`。
- [ ] `libra branch -m m m/m` 退出 0。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::ref_namespace_df_conflict_rename`（new）
**Full-suite trigger:** T-1: 修改共享 helper `src/internal/branch.rs`
**Dependencies:** BR-10, DEP-BR-08, DEP-BR-11, DEP-BR-12
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/internal/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=重命名D/F冲突检查与错误点名; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=BR-10,DEP-BR-08,DEP-BR-11,DEP-BR-12; writeset=序列化于BR-10; release=independent; split-from=BR-10; exception=N/A`

---

### Task BR-35: 已撤回（不得开工）

**Status:** superseded。Codex R39 判定 `REL-BR-10` 没有不可分割的中间态。创建、重命名、复制已分别由 `BR-10`、`BR-34`、`BR-36` 独立发布。本编号不再拥有写集、验收标准或发布边界，不计入 38 张活动卡。

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
- [ ] 当 `o/p` 存在时，`libra branch -c src o` 退出非零。
- [ ] 上一条失败的 stderr 点名 `o/p`。
- [ ] 当 `o` 存在时，`libra branch -c src o/p` 退出非零。
- [ ] 上一条失败的 stderr 点名 `o`。
- [ ] 当 `o/p` 存在时，`libra branch -C src o` 退出非零。
- [ ] 上一条失败的 stderr 点名 `o/p`。
- [ ] 当 `o` 存在时，`libra branch -C src o/p` 退出非零。
- [ ] 上一条失败的 stderr 点名 `o`。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::ref_namespace_df_conflict_copy`（new；覆盖 `o/p` 已存在和 `o` 已存在两个方向）
**Full-suite trigger:** T-1: 修改共享 helper `src/internal/branch.rs`
**Dependencies:** BR-34, DEP-BR-08, DEP-BR-11, DEP-BR-12
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/internal/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=复制D/F冲突检查与错误点名; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=BR-34,DEP-BR-08,DEP-BR-11,DEP-BR-12; writeset=序列化于BR-34; release=independent; split-from=BR-34; exception=N/A`

---

### Task BR-11: 修复 `libra branch --copy` 复制 reflog 与全量 `branch.*` 配置（#690）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #690：`copy_branch_impl` 在复制分支时同时复制源分支的 reflog（并追加 copy 记录）以及全部 `branch.<old>.*` 配置项（含 `description`、`pushRemote`、`rebase` 等，不仅限于 `remote`/`merge`）。
**Out of scope:** 同名复制 `m2 m2`（由 `BR-12` 承接）；`-c -f` 参数校验（由 `BR-13` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `copy_branch_impl` 硬编码仅复制 `["remote", "merge"]`，且未复制 reflog | `src/command/branch.rs:2372-2384` |
**Acceptance criteria:**
- [ ] `libra branch -c <old> <new>` 后，`<new>` 拥有 `<old>` 的完整 reflog 历史副本（且 `<old>` 的 reflog 保持不变）。
- [ ] 目标 `<new>` reflog 除历史副本外还包含本次 copy 操作记录。
- [ ] `branch.<old>.*` 下所有配置键值（如 `description`、`remote`、`merge`、`rebase`、`pushRemote` 等）均复制到 `branch.<new>.*`。
- [ ] 使用 `-C` 覆盖已有目标分支时，目标分支旧的 `branch.<new>.*` 配置与 reflog 被正确清理/替换为源分支副本。
- [ ] 复制或 `-C` 覆盖中间步骤（ref / metadata `MetadataKv` / config / reflog）发生失败时，原分支与目标分支的 ref、metadata、config 与 reflog 完整恢复至覆盖前状态（事务回滚或显式补偿），不留半写损坏状态。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::copy_preserves_reflog_and_all_config`（new；同时断言目标 reflog 含本次 copy 记录）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::copy_partial_failure_rollback`（new；故障注入验证中间失败时 ref/metadata/config/reflog 覆盖前状态完整恢复）
- [ ] `source .env.test && cargo nextest run --lib internal::reflog`
**Full-suite trigger:** T-1: 修改共享 reflog 存储模块 `src/internal/reflog.rs`
**Dependencies:** BR-36, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/internal/reflog.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--copy复制reflog与全量branch.*配置; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=2; scope=M; deps=BR-36,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-36; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-12: 支持 `libra branch -c` 同名分支复制成功语义（#691）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #691：当源分支与目标分支名相同时（如 `libra branch -c m2 m2` 或在 `m2` 上执行 `libra branch -c m2`），允许成功返回（no-op，不破坏分支/reflog/配置），与 Git 对齐。
**Out of scope:** `-c -f` 强制标志解析（由 `BR-13` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `copy_branch_impl` 在 `!force && destination_exists` 时无条件返回 `AlreadyExists` | `src/command/branch.rs:2344-2350` |
**Acceptance criteria:**
- [ ] `libra branch -c m2 m2`（以及当前在 `m2` 时 `libra branch -c m2`）退出 0，不报 `already exists`。
- [ ] 同名复制后 `m2` 的提交、reflog 与 `branch.m2.*` 配置保持完整无损。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::copy_same_name_succeeds`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-11, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-c同名复制成功语义; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-11,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-11; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-13: 支持 `libra branch -c -f` 强制复制（#692）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #692：修复 `BranchArgs.copy` 的 `num_args = 1..=2` 导致 `-c -f` 被 clap 提前报错（`a value is required for '--copy'`）的解析问题，并放开 `BranchArgs.force` 的 `requires = "new_branch"` 约束，使 `libra branch -c -f` / `--copy --force` 等价于 `-C`。
**Out of scope:** 无关联选项（如 `--list -f`）仍应拒绝。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `BranchArgs.copy` 声明 `num_args = 1..=2` 吞并/拒绝 `-c -f`，且 `force` 标记了 `requires = "new_branch"` | `src/command/branch.rs:348-349,435-436` |
**Acceptance criteria:**
- [ ] `libra branch -c -f <old> <new>` 与 `libra branch --copy --force <old> <new>` 成功强制覆盖已存在的 `<new>` 分支。
- [ ] 非法使用 `-f`（如 `libra branch -f` 无分支名或 `libra branch --list -f`）仍报错拒绝。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::copy_with_force_flag`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-12, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-c-f强制复制; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-12,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-12; release=independent; split-from=N/A; exception=N/A`

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
- [ ] `libra branch -i` 与 `libra branch -i --list <pattern>` 正常执行且与 `--ignore-case` 行为完全一致。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::short_ignore_case_flag`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-13, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-i短选项别名; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=1/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-13,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-13; release=independent; split-from=N/A; exception=N/A`

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
- [ ] 在 detached HEAD 下运行 `libra branch --show-current`，stdout 为空、stderr 为空、退出码为 0。
- [ ] 在附着分支与 unborn 分支下，`libra branch --show-current` 仍正常打印当前分支名。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::show_current_detached_silent`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-14, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--show-current在detachedHEAD静默; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-14,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-14; release=independent; split-from=N/A; exception=N/A`

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
- [ ] `libra branch --merged A --merged B` 被接受并列出合并进 A 或 B（并集）的分支。
- [ ] `libra branch --no-merged A --no-merged B` 被接受并过滤掉已合并进 A 或 B 的分支。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::repeated_merged_filters`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-15, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=重复--merged/--no-merged过滤; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-15,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-15; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-17: 支持 `libra branch --forked <pattern>` 分叉分支列表（#696）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #696（对应 Git `6de20f6092` `t/t3200-branch.sh#179`）：在 `BranchArgs` 中新增可重复的 `--forked=<pattern>`（必填分支/远程模式参数，隐含 `--list`），按分支**已配置的 upstream**（支持远程名与通配符解析）过滤本地分支，并支持附加位置 `<pattern>`、`--no-merged` 与 `--format` 组合。
**Out of scope:** 全局 `-C` 标志（由 `DEP-BR-03` / `#686` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `BranchArgs` 未定义 `--forked` 选项 | `src/command/branch.rs:331-508` |
| Git `6de20f6092` `t/t3200-branch.sh#179`：`--forked` 参数必填可重复，按 configured upstream 过滤，未知分支/模式报错 | `gh issue view 696` |
**Acceptance criteria:**
- [ ] `--forked <pattern>` 为必填参数，按分支已配置的 upstream 过滤本地分支（支持远程名与位置 `<pattern>` 进一步收窄）。
- [ ] 重复传入 `--forked`（含字面量与 glob 混用，如 `--forked local-base --forked "other/*"`）按**并集（union）**语义组合过滤（`t3200#1856-1875`）。
- [ ] `libra branch --forked "origin/*" --no-merged origin/one --format="%(refname:short)"` 在 `t3200#179` 拓扑下精确输出 `local-one`。
- [ ] 传入未知分支或无效模式时 fail-closed 报错退出非零。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::forked_branch_listing`（new；含重复 `--forked` 并集与混用断言）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-16, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--forked分叉分支列表; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-16,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-16; release=independent; split-from=N/A; exception=N/A`

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
- [ ] `libra branch --sort=objecttype` 与 `--sort=-objecttype` 均正常执行（不再报 `unsupported sort key 'objecttype'`）。
- [ ] 按分支指向对象的类型排序，同类型条目保持 refname 升序（支持 `-i`）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::sort_objecttype`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-17, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--sort=objecttype排序键; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-17,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-17; release=independent; split-from=N/A; exception=N/A`

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
- [ ] `libra branch --sort=refname --sort=-committerdate` 被接受，主序按 `-committerdate`，相等时按 `refname` 排序。
- [ ] `--sort=A --no-sort --sort=B` 仅保留 `B`；`--sort=A --no-sort` 回退默认 `refname` 排序。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::repeated_sort_keys`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-18, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=重复--sort多键排序; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-18,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-18; release=independent; split-from=N/A; exception=N/A`

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
- [ ] `libra branch --format='%(refname:short) %(ahead-behind:HEAD)'` 与 `libra for-each-ref --format='%(ahead-behind:<rev>)'` 正确输出 `<ahead> <behind>`（如 `2 1`、`0 0`）。
- [ ] `<committish>` 无效或无法解析时 fail-closed 报错并退出非零。
- [ ] 缺失 `<committish>` 参数（如 `%(ahead-behind:)`）返回明确格式错误。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::format_ahead_behind_atom`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::for_each_ref_test::ahead_behind_atom`（new）
**Full-suite trigger:** T-1: 修改共享格式化引擎 `src/command/for_each_ref.rs`
**Dependencies:** BR-19, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/for_each_ref.rs`, `src/command/branch.rs`, `tests/command/branch_test.rs`, `tests/command/for_each_ref_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/branch.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/for-each-ref.md`、`docs/commands/zh-CN/for-each-ref.md`、`docs/development/commands/branch.md`、`docs/development/commands/for-each-ref.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点 `src/command/`，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=%(ahead-behind)格式原子; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=1; prod-files=2; scope=M; deps=BR-19,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-19; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-21: 修复 `libra branch --format` 输出包含 detached HEAD 伪条目（#701）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #701：当处于 detached HEAD 状态运行 `libra branch --format=<fmt>` 时，在 `branch.rs` 与 `for_each_ref.rs` 协同支持 detached HEAD 伪条目渲染（`%(HEAD)` = `*`，`%(refname)` / `%(refname:short)` 按 HEAD reflog 区分 `(HEAD detached at <ref/oid>)` 与移动后的 `(HEAD detached from <ref/oid>)`，例如 `(HEAD detached from fromtag)`）。
**Out of scope:** `--show-current` detached HEAD 静默行为（已由 `BR-15` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `render_branch_output` 的 `--format` 分支未注入 `detached_head`，且 `for_each_ref::render_ref_format_lines` 在 detached HEAD 时设 `head_refname = None` | `src/command/branch.rs:2875-2899`、`src/command/for_each_ref.rs:1391-1410` |
**Acceptance criteria:**
- [ ] 在 detached HEAD 下未移动 HEAD 时，`libra branch --format='%(HEAD) %(refname) %(objectname:short)'` 输出 `%(HEAD)` 为 `*`。
- [ ] 在 detached HEAD 下未移动 HEAD 时，`%(refname)` 输出 `(HEAD detached at ...)`。
- [ ] 在 detached HEAD 下未移动 HEAD 时，`%(refname:short)` 输出 `(HEAD detached at ...)`。
- [ ] 从命名 ref detach 后再移动 HEAD 时，`%(refname)` 输出 `(HEAD detached from fromtag)`。
- [ ] 从命名 ref detach 后再移动 HEAD 时，`%(refname:short)` 输出 `(HEAD detached from fromtag)`。
- [ ] detached HEAD 伪条目在 `--format` 输出列表中保持完整 Git 顺序（位于普通分支之前）。
- [ ] 非 detached HEAD 状态下运行 `libra branch --format=...` 不额外注入 detached HEAD 伪条目。
- [ ] 传入 `-r`（仅远程）且无本地 HEAD 展示时不额外注入 detached HEAD 伪条目。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::format_includes_detached_head`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::for_each_ref_test`
**Full-suite trigger:** T-1: 修改共享格式化引擎 `src/command/for_each_ref.rs`
**Dependencies:** BR-20, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/command/for_each_ref.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/branch.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/for-each-ref.md`、`docs/commands/zh-CN/for-each-ref.md`、`docs/development/commands/branch.md`、`docs/development/commands/for-each-ref.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点 `src/command/`，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--format含detachedHEAD伪条目; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=2; scope=M; deps=BR-20,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-20; release=independent; split-from=N/A; exception=N/A`

---

### Task BR-22: 支持 `libra branch -r -d`/`-D` 删除远程追踪分支（#866 部分 1/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #866 中 `branch -r -d` 部分：解除 clap `action`/`query` 组对 `-r` + `-d`/`-D` 的错误互斥（仅允许 `-r` 与 `-d`/`-D` 或列表组合），解析 `<remote>/<branch>`（及 `refs/remotes/<remote>/<branch>`）并删除对应的远程追踪引用及关联 reflog。
**Out of scope:** `fetch --all` 跳过 `remote.<name>.skipFetchAll`（已自本卡拆出，由 `BR-24` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `ArgGroup::new("action").conflicts_with("query")` 在 clap 层拒绝 `-r -d`，且 `delete_branch_impl` 仅查本地分支 | `src/command/branch.rs:320-330,2171-2233` |
| `Branch::delete_branch_result` 已接受 `remote: Option<&str>` 参数 | `src/internal/branch.rs:829-890` |
**Acceptance criteria:**
- [ ] `libra branch -r -d origin/feat` 与 `libra branch -r -D origin/feat` 成功删除 `refs/remotes/origin/feat` 并输出 `Deleted remote-tracking branch origin/feat (was <short>).`。
- [ ] 删除远程追踪分支时，不触发本地分支的 metadata 级联删除，且不影响同名本地分支 `feat`。
- [ ] 目标远程追踪分支不存在时，返回 exit 1 且提示 `remote-tracking branch '<name>' not found`（不误删）。
- [ ] 在 ref 删除后、reflog 删除前注入失败时，远程追踪 ref、reflog 与本地元数据均保持原状，事务回滚且不留下部分删除。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::delete_remote_tracking_branch`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::delete_remote_tracking_branch_rolls_back_midway_failure`（new；在 ref 删除后、reflog 删除前注入失败，逐项比较 ref/reflog/metadata 前后字节与记录）
- [ ] `source .env.test && cargo nextest run --lib internal::branch`
**Full-suite trigger:** T-1: 修改共享分支存储模块 `src/internal/branch.rs`
**Dependencies:** BR-21, DEP-BR-08, DEP-BR-11, DEP-BR-12
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `src/internal/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-r-d删除远程追踪分支; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=2; prod-files=2; scope=M; deps=BR-21,DEP-BR-08,DEP-BR-11,DEP-BR-12; writeset=序列化于BR-21; release=independent; split-from=N/A; exception=N/A`

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
- [ ] `libra branch --sort=ahead-behind:HEAD` 与 `--sort=-ahead-behind:HEAD` 正常执行，排序结果与 Git `t3203#33` 升/降序预期一致。
- [ ] `<committish>` 无效或缺失时 fail-closed 返回明确错误。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::sort_ahead_behind`（new）
- [ ] `source .env.test && cargo nextest run --lib command::branch::tests`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-22, DEP-BR-08, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/command/branch.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--sort=ahead-behind排序键; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-22,DEP-BR-08,DEP-BR-11; writeset=序列化于BR-22; release=independent; split-from=BR-20; exception=N/A`

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
- [ ] 配置 `remote.<name>.skipFetchAll=true` 后，运行 `libra fetch --all` 跳过 `<name>` 并正常拉取其余未设置该项的远端。
- [ ] 直接运行 `libra fetch <name>` 时仍正常拉取 `<name>`（不受 `skipFetchAll` 阻止）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::fetch_test::skip_fetch_all`（new）
- [ ] `source .env.test && cargo nextest run --lib command::fetch`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** BR-23, DEP-BR-08, DEP-BR-10
**Deliverables:** N/A
**Implementation write set:** `src/command/fetch.rs`, `tests/command/fetch_test.rs`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md` 与 `COMPATIBILITY.md` 中 `remote.<name>.skipFetchAll` 说明。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=fetch--all尊重skipFetchAll; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-23,DEP-BR-08,DEP-BR-10; writeset=序列化于BR-23; release=independent; split-from=BR-22; exception=N/A`

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
- [ ] 配置 `remote.<r>.push` 时在 `BR-26` 落地前 fail-closed 返回包含该配置名的过渡拒绝，而不是笼统的“尚不支持 `@{push}`”。
- [ ] 配置 `remote.<r>.mirror=true` 时在 `BR-26` 落地前 fail-closed 返回包含 `mirror` 的过渡拒绝。
- [ ] 未显式配置 `push.default`（默认 `simple`）时在 `BR-27` 落地前 fail-closed 返回包含 `simple` 的过渡拒绝。
- [ ] 显式配置 `push.default=simple` 时在 `BR-27` 落地前 fail-closed 返回包含 `simple` 的过渡拒绝。
- [ ] `push.default=nothing` 时在 `BR-26` 落地前 fail-closed 返回包含 `nothing` 的过渡拒绝。
- [ ] `push.default=matching` 时在 `BR-26` 落地前 fail-closed 返回包含 `matching` 的过渡拒绝。
- [ ] `push.default=current` 时在 `BR-26` 落地前 fail-closed 返回包含 `current` 的过渡拒绝。
- [ ] `push.default=upstream` 时在 `BR-25` 落地前 fail-closed 返回包含 `upstream` 的过渡拒绝，不得在两次发布之间走未定义回退。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::rev_parse_test::push_transition_fail_closed_guards`（new；先验证最小 `@{push}` 入口可识别，再逐项断言 7 个未实现配置及 `push.default=upstream` 各自可区分的过渡拒绝，且错误不是笼统“尚不支持 `@{push}`”）
**Full-suite trigger:** T-1: 修改共享修订解析 `src/utils/util.rs`
**Dependencies:** BR-24, DEP-BR-08, DEP-BR-09
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/revision.md`、`docs/commands/zh-CN/revision.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md` 与 `COMPATIBILITY.md`（登记 7 项未实现配置及 `push.default=upstream` 的过渡 fail-closed 边界与后续清除责任）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{push}过渡fail-closed守卫; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-24,DEP-BR-08,DEP-BR-09; writeset=序列化于BR-24; release=independent; split-from=BR-25; exception=N/A`

---

### Task BR-25: 支持 `@{push}` remote 选择与 `upstream`（#684 部分 4/9）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #684 中 `@{push}` 基础解析与 remote 选择轴（自 `BR-06` 拆出）：在 `BR-40` 已交付的最小 `@{push}` 入口上扩展当前分支的 `pushRemote` → `pushDefault` → `branch.remote` → `origin` 选择与 `push.default=upstream`，而不是重新新增一套解析器。7 项过渡 fail-closed 守卫已前置到 `BR-40`，本卡不再合并该门族。
**Out of scope:** `@{upstream}`/`@{u}`（已在 `BR-06`/`BR-32`/`BR-31` 交付）；过渡守卫（`BR-40`）；`<branch>@{push}`（`BR-29`）；`simple` 与 `tracking_for_push_dest`（`BR-27`）；detached HEAD（`BR-30`）；`nothing`/`matching`/`current`/refspec/`mirror`（`BR-26`）；`%(push)`（`BR-28`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `util.rs:2649` 尚未支持 `@{push}` 修订解析 | `src/utils/util.rs:2649`、`src/command/for_each_ref.rs:250-262` |
**Acceptance criteria:**
- [ ] `@{push}` 在 `push.default=upstream` 且所选 push remote 与 upstream remote 一致时解析为当前分支上游追踪分支所指向的提交。
- [ ] 所选 push remote 与 upstream remote 不一致时按 Git 行为报错，不返回另一远端的追踪引用或提交。
- [ ] 按 `branch.<name>.pushRemote` → `remote.pushDefault` → `branch.<name>.remote` → `origin` 顺序选择推送 remote。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::rev_parse_test::push_remote_and_upstream`（new；覆盖同 remote 成功、跨 remote 失败及 remote 选择优先级）
**Full-suite trigger:** T-1: 修改共享修订解析 `src/utils/util.rs`
**Dependencies:** BR-40, DEP-BR-08, DEP-BR-09
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/revision.md`、`docs/commands/zh-CN/revision.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md` 与 `COMPATIBILITY.md`（注明 upstream remote 一致性与 remote 选择优先级，移除 `BR-40` 对 `push.default=upstream` 的过渡拒绝，其他过渡拒绝由后续卡按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，公开修订语法）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{push}remote选择与upstream; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=BR-40,DEP-BR-08,DEP-BR-09; writeset=序列化于BR-40; release=independent; split-from=BR-06; exception=N/A`

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
- [ ] `<branch>@{push}` 解析指定分支的推送目标 ref。
- [ ] `<branch>@{push}` 解析指定分支的推送目标提交。
- [ ] `@{push}` 的字母大小写不影响解析结果。
- [ ] `libra branch -f <name> @{push}` 把 `<name>` 更新到 `@{push}` 解析出的提交。
- [ ] `libra branch --set-upstream-to=@{push}` 把当前分支 upstream 设为 `@{push}` 解析出的分支。
- [ ] `libra branch -f @{push} <commit>` 把解析出的本地分支指向 `<commit>`。
- [ ] `@{push}` 解析到远程追踪 ref 时，`libra branch -f @{push} <commit>` 退出非零。
- [ ] 上一条拒绝之后，已有 ref 与执行前相同。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::rev_parse_test::push_explicit_branch_and_case`（new；同时验证 `<branch>@{push}` 目标 ref 与提交）
- [ ] `source .env.test && cargo nextest run --test command_test command::branch_test::branch_push_name_interpretation`（new；覆盖起点槽、分支名槽本地更新，以及远程追踪目标拒绝且不改写）
**Full-suite trigger:** T-1: 修改共享修订解析 `src/utils/util.rs`
**Dependencies:** BR-25, DEP-BR-08, DEP-BR-09, DEP-BR-11
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `src/command/branch.rs`, `tests/command/rev_parse_test.rs`, `tests/command/branch_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/branch.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/commands/revision.md`、`docs/commands/zh-CN/revision.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/branch.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件，公开修订与分支名解释语法）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{push}显式分支与大小写及分支名解释; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-25,DEP-BR-08,DEP-BR-09,DEP-BR-11; writeset=序列化于BR-25; release=independent; split-from=BR-25; exception=N/A`

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
- [ ] 未显式配置 `push.default`（默认 `simple`）下 `tracking_for_push_dest(remote, branch) == upstream` 时解析成功。
- [ ] 显式配置 `push.default=simple` 下 `tracking_for_push_dest(remote, branch) == upstream` 时解析成功。
- [ ] `tracking_for_push_dest` 经 fetch refspec 正确映射 push 目标到对应本地远程追踪分支。
- [ ] 当前分支缺少 upstream 配置时返回 upstream 缺失错误（先于 `cannot resolve 'simple' push` 检查，对齐 Git `remote.c:2013`）。
- [ ] `tracking_for_push_dest` 无对应本地追踪分支时报错 `has no local tracking branch`。
- [ ] `simple` 下同远端不同名映射（双方均存在但不同）报错 `cannot resolve 'simple' push to a single destination`。
- [ ] `simple` 下异远端三角配置（双方均存在但不同）报错 `cannot resolve 'simple' push to a single destination`。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::rev_parse_test::push_simple_and_tracking`（new；含 `t1514` `default_unset_simple`、`explicit_simple`、`missing_upstream`、`no_local_tracking_branch`、`same_remote_mismatch`、`triangular_fail`）
- [ ] `source .env.test && cargo nextest run --lib utils::util`
**Full-suite trigger:** T-1: 修改共享修订解析 `src/utils/util.rs`
**Dependencies:** BR-29, DEP-BR-08, DEP-BR-09
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`docs/commands/revision.md`、`docs/commands/zh-CN/revision.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md` 与 `COMPATIBILITY.md`（`branch --set-upstream-to=@{push}` 使用本卡扩展后的共享解析器，同卡更新 `branch` 行为说明）（同步清除 `BR-40` 遗留的未配置 `push.default` 与 `push.default=simple` fail-closed 过渡说明，按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，公开 `simple` 修订契约）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{push}simple与tracking_for_push_dest; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-29,DEP-BR-08,DEP-BR-09; writeset=序列化于BR-29; release=independent; split-from=BR-25; exception=N/A`

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
- [ ] 需要从 `HEAD` 推断分支（`@{push}`）而 `HEAD` 处于 detached 状态时 fail-closed 报错。
- [ ] detached HEAD 下显式 `<branch>@{push}` 仍按 `<branch>` 配置正常解析。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::rev_parse_test::push_detached_head`（new；含 `detached_head_fail` 与 `detached_explicit_branch_ok`）
- [ ] `source .env.test && cargo nextest run --lib utils::util`
**Full-suite trigger:** T-1: 修改共享修订解析 `src/utils/util.rs`
**Dependencies:** BR-27, DEP-BR-08, DEP-BR-09
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`docs/commands/revision.md`、`docs/commands/zh-CN/revision.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md` 与 `COMPATIBILITY.md`（`branch --set-upstream-to=@{push}` 使用本卡扩展后的共享解析器，同卡更新 `branch` 行为说明）（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，公开 detached HEAD `@{push}` 契约）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=@{push}detachedHEAD规则; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-27,DEP-BR-08,DEP-BR-09; writeset=序列化于BR-27; release=independent; split-from=BR-27; exception=N/A`

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
- [ ] `push.default=nothing` 时报错 `push has no destination (push.default is 'nothing')`。
- [ ] `push.default=current` 映射到 push remote 同名分支。
- [ ] `push.default=matching` 按 Git 规则映射到 push remote 同名分支。
- [ ] `remote.<r>.push` 自定义 refspec 按配置映射推送目标。
- [ ] `remote.<r>.push` 无匹配规则时报错 `push refspecs ... do not include`。
- [ ] `remote.<r>.mirror=true` 按镜像规则映射并经 `tracking_for_push_dest` 查找本地追踪分支。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::rev_parse_test::push_modes_and_refspec`（new；含 `t1514` `default_current`、`default_matching`、`nothing_fail`、`custom_refspec`、`mirror`）
- [ ] `source .env.test && cargo nextest run --lib utils::util`
**Full-suite trigger:** T-1: 修改共享修订解析 `src/utils/util.rs`
**Dependencies:** BR-30, DEP-BR-08, DEP-BR-09
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`docs/commands/revision.md`、`docs/commands/zh-CN/revision.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md` 与 `COMPATIBILITY.md`（`branch --set-upstream-to=@{push}` 使用本卡扩展后的共享解析器，同卡更新 `branch` 行为说明）（同步清除 `BR-40` 遗留的全部 `nothing`/`matching`/`current`/refspec/`mirror` fail-closed 过渡说明，按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，公开修订语法与模式支持）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=nothing/matching/current/refspec/mirror; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=BR-30,DEP-BR-08,DEP-BR-09; writeset=序列化于BR-30; release=independent; split-from=BR-25; exception=N/A`

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
- [ ] `for-each-ref --format='%(push)'` 复用 `util.rs` 共享解析器并输出与 Git `%(push)` 一致的结果。
- [ ] `%(push)` 在 `simple` 三角失败等无效场景输出空串（不输出 `refs/remotes/...`）。
- [ ] `%(push)` 正确透传配置读取错误。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::for_each_ref_test::push_atom_custom_refspec_and_errors`（new）
- [ ] `source .env.test && cargo nextest run --lib command::for_each_ref`
**Full-suite trigger:** T-1: 修改共享格式化引擎 `src/command/for_each_ref.rs` 与 `src/utils/util.rs`
**Dependencies:** BR-26, DEP-BR-08, DEP-BR-09
**Deliverables:** N/A
**Implementation write set:** `src/utils/util.rs`, `src/command/for_each_ref.rs`, `tests/command/for_each_ref_test.rs`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/for-each-ref.md`、`docs/commands/zh-CN/for-each-ref.md`、`docs/development/commands/for-each-ref.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md` 与 `COMPATIBILITY.md`（按 ER-06/ER-06a 逐文件核对与验收）。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件，公开格式原子对齐）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=for-each-ref%(push)复用与错误透传; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=BR-26,DEP-BR-08,DEP-BR-09; writeset=序列化于BR-26; release=independent; split-from=BR-26; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `BranchArgs` 校验、`sort_branch_entries`、`resolve_upstream_spec`、`reflog` 迁移/复制 | `source .env.test && cargo nextest run --lib command::branch::tests` |
| 集成 | 全部 21 个 Issue 的 CLI 行为、退出码、stderr/stdout、D/F 碰撞与 `skipFetchAll` | `source .env.test && cargo nextest run --test command_test -E 'test(/^command::(branch_test|rev_parse_test|for_each_ref_test|fetch_test)::/)'` |
| 兼容 | 版本面同步与 `git branch`/`fetch` 对照语义 | `source .env.test && cargo nextest run --test compat_version_surface_sync` |
| 迁移 | N/A：无 SQLite schema 迁移 | N/A |
| 安全 | 跨 worktree 分支保护与不误删本地元数据仍由集成行的 `branch_test` 覆盖。D/F 两个方向分别是：`BR-10` `ref_namespace_df_conflict_create`、`BR-34` `ref_namespace_df_conflict_rename`、`BR-36` `ref_namespace_df_conflict_copy` | `source .env.test && cargo nextest run --test command_test command::branch_test::ref_namespace_df_conflict_create && source .env.test && cargo nextest run --test command_test command::branch_test::ref_namespace_df_conflict_rename && source .env.test && cargo nextest run --test command_test command::branch_test::ref_namespace_df_conflict_copy` |
| 性能 | 继承 GC-10，列表/排序无无界扫描 | N/A |
| live/gated | 计划收口双环境全量门 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| BR-01 | Issue #678 / `branch.rs:491-508` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::column_verbose_conflict` |
| BR-02 | Issue #679 / `branch.rs:320-365` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::list_pattern` |
| BR-03 | Issue #680 / `branch.rs:2949-2967`; `output.rs:154-205` | `src/cli.rs`, `src/utils/output.rs`, `src/command/branch.rs` | `docs/commands/branch.md` EN+zh+dev+网站、`COMPATIBILITY.md` | `color_branch_mode_cascade`、`color_branch_cli_override_including_explicit_auto`、`color_branch_auto_tty_enables_color`、`color_branch_auto_redirect_no_ansi`、`color_branch_never_and_no_color_flag_no_ansi`、`color_branch_no_color_env_precedence`、`color_branch_term_dumb_precedence`、`color_branch_output_config_and_non_branch_regression` |
| BR-33 | 已撤回 | 无写集 | 无 | 基线已覆盖，不新增测试卡 |
| BR-37 | Issue #680 / `branch.rs:2880` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh+dev+网站、`COMPATIBILITY.md` | `color_branch_slot_current`、`color_branch_slot_local`、`color_branch_slot_remote`、`color_branch_slot_upstream`、`color_branch_slot_plain`、`color_branch_slot_upstream_plain_segments`、`color_branch_slot_never_no_ansi`、`color_branch_slot_auto_redirect_no_ansi` |
| BR-38 | Issue #680 / `branch.rs:2880-2967` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh+dev+网站、`COMPATIBILITY.md` | `color_branch_render_default_and_custom`、`color_branch_unborn_default_and_custom`、`color_branch_detached_default_and_custom`、`color_branch_column_default_and_custom` |
| BR-39 | `REL-BR-03` / #680 | 发布面（`Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`） | `REL-BR-03` 发布说明与家族网站交付 | `command_test color_branch_`、`compat_version_surface_sync`、双环境 `T-2` 全量 |
| BR-04 | Issue #681 / `branch.rs:1400-1432` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::set_upstream_refs_heads` |
| BR-05 | Issue #682 / `branch.rs:1675-1687` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md`、`docs/error-codes.md` | `command::branch_test::unset_upstream_without_config` |
| BR-06 | Issue #684 / `util.rs:2635-2675` | `src/utils/util.rs`、`src/command/branch.rs` | `branch/revision/rev-parse.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::branch_upstream_name_interpretation` |
| BR-32 | Issue #684 / `util.rs:2635-2675` | `src/utils/util.rs`、`src/command/branch.rs` | `branch/revision/rev-parse.md` EN+zh、`COMPATIBILITY.md` | `command::rev_parse_test::upstream_short_and_case_variants` |
| BR-31 | Issue #684 / `util.rs:2635-2675` | `src/utils/util.rs`、`src/command/branch.rs` | `branch/revision/rev-parse.md` EN+zh、`COMPATIBILITY.md` | `command::rev_parse_test::upstream_detached_head` |
| BR-07 | Issue #685 / `util.rs:2649-2675` | `src/utils/util.rs`、`src/command/branch.rs` | `branch/revision/rev-parse.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::branch_previous_branch_upstream` |
| BR-08 | Issue #687 / `branch.rs:429-430` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md`、`docs/error-codes.md` | `command::branch_test::rename_missing_arg_exit_128` |
| BR-09 | Issue #688 / `branch.rs:2282-2316` | `src/command/branch.rs`、`src/internal/reflog.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::rename_moves_reflog`、`command::branch_test::rename_partial_failure_rollback`、`command::branch_test::rename_force_overwrite_target_rolls_back`、`command::branch_test::rename_force_overwrite_target_succeeds` |
| BR-10 | Issue #689 / `branch.rs:2274-2280` | `src/command/branch.rs`、`src/internal/branch.rs` | `docs/commands/branch.md` EN+zh+dev+网站、`COMPATIBILITY.md` | `command::branch_test::ref_namespace_df_conflict_create` |
| BR-34 | Issue #689 / `branch.rs:2274-2280` | `src/command/branch.rs`、`src/internal/branch.rs` | `docs/commands/branch.md` EN+zh+dev+网站、`COMPATIBILITY.md` | `command::branch_test::ref_namespace_df_conflict_rename` |
| BR-35 | 已撤回 | 无写集 | 无 | 不新增测试 |
| BR-36 | Issue #689 / `branch.rs:2344-2350` | `src/command/branch.rs`、`src/internal/branch.rs` | `docs/commands/branch.md` EN+zh+dev+网站、`COMPATIBILITY.md` | `command::branch_test::ref_namespace_df_conflict_copy` |
| BR-11 | Issue #690 / `branch.rs:2372-2384` | `src/command/branch.rs`、`src/internal/reflog.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::copy_preserves_reflog_and_all_config` |
| BR-12 | Issue #691 / `branch.rs:2344-2350` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::copy_same_name_succeeds` |
| BR-13 | Issue #692 / `branch.rs:348-349` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::copy_with_force_flag` |
| BR-14 | Issue #693 / `branch.rs:480-482` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::short_ignore_case_flag` |
| BR-15 | Issue #694 / `branch.rs:3012-3024` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::show_current_detached_silent` |
| BR-16 | Issue #695 / `branch.rs:463-470` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::repeated_merged_filters` |
| BR-17 | Issue #696 / `gh issue view 696` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::forked_branch_listing` |
| BR-18 | Issue #699 / `branch.rs:3404-3416` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::sort_objecttype` |
| BR-19 | Issue #700 / `branch.rs:477-478` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::repeated_sort_keys` |
| BR-20 | Issue #700 / `for_each_ref.rs:1580-1615` | `src/command/for_each_ref.rs`、`src/command/branch.rs` | `branch.md` & `for-each-ref.md` EN+zh+web、`COMPATIBILITY.md` | `command::for_each_ref_test::ahead_behind_atom` |
| BR-21 | Issue #701 / `for_each_ref.rs:1391-1410` | `src/command/branch.rs`、`src/command/for_each_ref.rs` | `branch.md` & `for-each-ref.en.md`、`COMPATIBILITY.md` | `command::branch_test::format_includes_detached_head` |
| BR-22 | Issue #866 / `branch.rs:320-330,2171` | `src/command/branch.rs`、`src/internal/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::delete_remote_tracking_branch`、`command::branch_test::delete_remote_tracking_branch_rolls_back_midway_failure` |
| BR-23 | Issue #700 / `branch.rs:3341,3404` | `src/command/branch.rs` | `docs/commands/branch.md` EN+zh、`COMPATIBILITY.md` | `command::branch_test::sort_ahead_behind` |
| BR-24 | Issue #866 / `src/command/fetch.rs` | `src/command/fetch.rs` | `docs/commands/fetch.md` EN+zh+web、`COMPATIBILITY.md` | `command::fetch_test::skip_fetch_all` |
| BR-40 | Issue #684 / `util.rs:2649` | `src/utils/util.rs` | `revision/rev-parse.md` EN+zh+web、`COMPATIBILITY.md` | `command::rev_parse_test::push_transition_fail_closed_guards` |
| BR-25 | Issue #684 / `util.rs:2649` | `src/utils/util.rs` | `revision/rev-parse.md` EN+zh+web、`COMPATIBILITY.md` | `command::rev_parse_test::push_remote_and_upstream` |
| BR-29 | Issue #684 / `util.rs:2649` | `src/utils/util.rs`、`src/command/branch.rs` | `branch/revision/rev-parse.md` EN+zh+web、`COMPATIBILITY.md` | `command::branch_test::branch_push_name_interpretation` |
| BR-27 | Issue #684 / `util.rs:2649` | `src/utils/util.rs` | `branch.md` 与 `revision/rev-parse.md` EN+zh+dev+web、`COMPATIBILITY.md` | `command::rev_parse_test::push_simple_and_tracking` |
| BR-30 | Issue #684 / `util.rs:2649` | `src/utils/util.rs` | `branch.md` 与 `revision/rev-parse.md` EN+zh+dev+web、`COMPATIBILITY.md` | `command::rev_parse_test::push_detached_head` |
| BR-26 | Issue #684 / `util.rs:2649` | `src/utils/util.rs` | `branch.md` 与 `revision/rev-parse.md` EN+zh+dev+web、`COMPATIBILITY.md` | `command::rev_parse_test::push_modes_and_refspec` |
| BR-28 | Issue #684 / `for_each_ref.rs:250` | `src/utils/util.rs`、`src/command/for_each_ref.rs` | `for-each-ref.md` EN+zh+web、`COMPATIBILITY.md` | `command::for_each_ref_test::push_atom_custom_refspec_and_errors` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划评审 PASS，基线冻结（`62abb622...`，`0.30.207`） | Codex `PASS` | N/A |
| M1 | Phase 0 (`BR-01`..`BR-05` 与 `BR-37`/`BR-38`/`BR-39`) 完成且发布 | 5 个 patch release（含 `REL-BR-03`）+ D 组全绿 | 按 `immutable-release` 前滚修复 |
| M2 | Phase 1 完成。发布切片是 `BR-06`..`BR-15`、`BR-31`、`BR-32`、`BR-34`、`BR-36`（含 `BR-10`；`BR-35` 不发布） | 14 个发布切片 + nextest 全绿 | 按 `immutable-release` 前滚修复 |
| M3 | Phase 2 (`BR-16`..`BR-30`) 及收口全量门全绿 | 全部 38 张活动卡 `done/complete`（含 `BR-34`、`BR-36`..`BR-40`，不含已撤回的 `BR-33` 与 `BR-35`） | 按 `immutable-release` 前滚修复 |

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
| `/Volumes/Sea/libra-backend` 非标准 Git 目录导致写入前置受阻 | 低：阻塞网站文档自动同步 | 严格按 ER-06a 核对 VCS 与 `cf` 分支，遇阻立即 STOP 报告 | 全部 `BR-*` |
| 创建、重命名或复制只交付退出码、不交付错误文本 | 中：用户看到非零退出但不知道碰撞的是哪条 ref | `BR-10`、`BR-34`、`BR-36` 各自把检查和 stderr 点名放在同一张可独立发布的卡里 | `BR-10`, `BR-34`, `BR-36` |
| 上游 `t3200#179` 依赖全局 `-C`（`#686`） | 低：仅影响外部完整脚本跑通 | 已登记 `DEP-BR-03`；本卡 focused 集成测试独立可跑 | `BR-17` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 分支列表与多键 `--sort` | $O(N \log N)$（$N$ 为分支数） | 单次调用线性预加载 commit 元信息 | 非验收观测目标：千分支规模，记录运行环境、样本数、median 与 spread；无数值通过门，G-10 仅禁止无界扫描/内存 | 非门禁观测，不设数值 PASS/FAIL；与测试矩阵 `Performance budget=N/A` 一致 |
| `%(ahead-behind:...)` 计算与排序 | 复用 `upstream_ahead_behind` | 按展示条目数线性调用 | 无无界全库扫描 | `command::for_each_ref_test` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已随各卡同步更新 `branch`、`for-each-ref`、`rev-parse`、`fetch` 行。
- [ ] `docs/commands/{branch,for-each-ref,revision,rev-parse,fetch}.md` 与对应 `zh-CN/` 文档已同步。
- [ ] `docs/development/commands/{branch,rev-parse,for-each-ref,fetch}.md` 已同步。
- [ ] `docs/error-codes.md` 已核对/同步。
- [ ] `tests/INDEX.md` 与 `Cargo.toml` `[[test]]` 已核对（复用既有 `--test command_test`）。
- [ ] `plan-long.md` 日期计划索引已同步或确认 `N/A`。

## Codex review log

Result 只允许 `PASS` 或 `FAIL`。`FAIL` 必须列出 P0/P1 条目并在下一轮复审关闭；P2 可由具名责任人书面接受为 residual risk，但不改变本轮 `FAIL` 记录（ER-05）。

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
| R10 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P2: 「成功定义」仍写 `25 张任务卡`（Claude R10 遇到 `ECONNRESET` 网络中断） | R11 全部同步关闭 | Codex R10 只读评审 |
| R11 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: `BR-25`→`25,27`、`BR-26`→`26,28` 拆分至原子 AC≤8；`BR-26` 补齐 `branch`/`revision` 文档；`BR-06` 补远程 `@{upstream}` 拒绝（`t3204#4`）；`BR-07` 明确 `branch.upstream-other.merge`；`BR-21` 补 `for-each-ref.md`；澄清 `#866` `t5514#9` 由 `BR-24` 完整覆盖 | R12 全部同步关闭 | Claude/Codex R11 只读评审 |
| R12 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R12 `FAIL` P1: `BR-25` 需对未配置 `push.default`（默认 `simple`）及 `nothing`/`matching`/`current` 在 `BR-27`/`BR-26` 落地前显式 fail-closed；`发布窗口顺序` 需展开 `BR-25 → BR-27 → BR-26 → BR-28`（Claude R12 `ECONNRESET`） | R13 全部同步关闭 | Codex R12 只读评审 |
| R13 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R13 `FAIL` P1: `BR-25` AC8 合并 4 种过渡配置导致实际谓词 11/8 超出 G-03 上限（Claude R13 `ECONNRESET`） | R14 全部同步关闭 | Codex R13 只读评审 |
| R14 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R14 `FAIL` P1: `BR-25` AC1 含 ref 与 commit 双谓词；缺显式 `push.default=simple` 过渡 AC；计划级 `T-1` 默认覆盖漏 `BR-29` | R15 全部同步关闭 | Codex R14 只读评审 |
| R15 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R15 `FAIL` P1: `BR-27` 依赖仍在 `BR-26` 的 `tracking_for_push_dest` 且缺 upstream/tracking 缺失独立错误；`BR-29` AC1 含 ref+commit 实为 `4/8`；`BR-17` 缺重复 `--forked` 并集语义；Claude R14 `FAIL` P1: `#684` 缺 `@{UPSTREAM}`/`@{U}` 大小写不敏感；P2: 同步 `评审结论`、`ADR-BR-02`、`BR-27`/`BR-26` 写集（Claude R15 `ECONNRESET`） | R16 全部同步关闭 | Codex R15 & Claude R14 只读评审 |
| R16 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R16 `FAIL` P1: `BR-06` detached HEAD 下应允许显式 `<branch>@{upstream}`；`BR-09`/`BR-11` 缺中间变更失败原子回滚/补偿 AC 与故障注入测试（Claude R16 `ECONNRESET`） | R17 全部同步关闭 | Codex R16 只读评审 |
| R17 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R17 `FAIL` P1: `BR-11` 回滚 AC 与测试漏目标分支 `metadata`（`MetadataKv`，`branch.rs:2392`）；`DEP-BR-08` 需明确为 `BR-01..BR-28` 计划级串行编辑锁（未获锁前阻塞编辑共享文件，接入 `BR-05`/`08`）且不预设 `plan-status.md` 已登记 | R18 全部同步关闭 | Codex R17 只读评审 |
| R18 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R18 `FAIL` P1: `DEP-BR-08` 锁区间漏 `BR-29`；网站文档路径须统一为 `/Volumes/Sea/libra-backend`（`cf` 分支，ER-06a） | R19 全部同步关闭 | Codex R18 只读评审 |
| R19 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: `DEP-BR-08` 须逐卡接入全部 29 张卡；`BR-25`/`29` 等 `Docs and compatibility impact` 须按 ER-06a 逐文件展开；P2: 修正 R18 修订历史措辞与 `EX-BR-01` 具名审批人 | R20 全部同步关闭 | Claude/Codex R19 只读评审 |
| R20 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: `BR-06` 需拆出 detached HEAD 规则并细化原子 AC（拆出 `BR-31`）；`BR-27` 需拆分未配置 `push.default` 与显式 `push.default=simple`（拆出 `BR-30`）；`BR-20`/`BR-21` 需按 ER-06a 展开完整逐文件文档清单（含 `BR-20` `for-each-ref.md`）；P2: 清理重复文档碎片、统一 `split-from` 谱系、修正 R19 日志 `P2` | R21 全部同步关闭 | Claude/Codex R20 只读评审 |
| R21 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | P1: `BR-06` 拆出 `BR-32`（`@{u}` 与大小写）达到严格单谓词；全部卡补齐同卡 ER-06/ER-06a 文档交付验收门；`DEP-BR-08`/`发布窗口顺序`/追溯表（`BR-31`/`32`/`30` 及 `BR-27` 测试名）全量同步 32 卡；P2: 同步 `评审结论`、`#684` `1/9`..`9/9`、清理 `BR-28` 碎片与 `BR-01`/`14` AC 计数 | R22 全部同步关闭 | Claude/Codex R21 只读评审 |
| R22 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R22 `FAIL` P1: `BR-32`/`31` 拆分单谓词；Claude R22 `FAIL` P1: 补齐跨计划共享源码依赖 `DEP-BR-09..12`、网站路径改为本地可用的 `/Volumes/Sea/libra-backend`（`cf` 分支）；P2: `BR-30` 过渡说明、`BR-29` AC4、`BR-21` `%(refname)`、`EX-BR-01` R14 | R23/R24 全部同步关闭 | Claude & Codex R22 只读评审 |
| R24 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R24 `FAIL` P1: `DEP-BR-02` 需明确 `/Volumes/Sea/libra-backend` 与执行机 `/Volumes/Sea/libra-backend` 的授权核对规则；`BR-21` 需拆细 `%(HEAD)=*`/`%(refname)`/顺序/排除（Claude R24 `ECONNRESET`） | R25 全部同步关闭 | Codex R24 只读评审 |
| R25 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R25 `FAIL` P1: `BR-21` AC1/AC2 需拆细至严格单谓词（`8/8`）；移除无独立评审的 `R23` 日志占位行（Claude R25 `ECONNRESET`） | R26 全部同步关闭 | Codex R25 只读评审 |
| R26 (Codex gpt-6.1-terra + Claude claude-opus-5-5) | `plan-20261117-branch.md` | FAIL | Codex R26 `FAIL` P1: 网站交付必须固定为 `/Volumes/Sea/libra-backend` 且不可用则 STOP；`BR-27`/`BR-30`/`BR-26` 须同卡更新 `branch` 文档（Claude R26 `ECONNRESET`） | R27 全部同步关闭 | Codex R26 只读评审 |
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
| R43 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R43 FAIL P1: `BR-03` 发布后 `auto` 没有契约（Claude 未跑） | `2026-11-17 11:28:33 UTC`：`BR-03` 把 `auto` 做成 `never`，`plain` 改由 `BR-37` 交付 | Codex R43 只读评审
| R44 (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | Codex R44 FAIL P1: `auto` 不能先发成 `never`；`plain` 与终端轴混卡（Claude 未跑） | `2026-11-17 11:38:06 UTC`：`BR-03` 一次交付完整 `auto`，`BR-37` 只交付五个颜色槽 | Codex R44 只读评审
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
| R55-B (Codex gpt-6.1-terra) | `plan-20261117-branch.md` 第 1001 行至文件末及指定交叉引用 | PASS | 无 P0/P1/P2。已核对 `BR-40` upstream 过渡拒绝与 `BR-25` 清除责任、`BR-22` 回滚及性能非门禁表述 | 分区结论，不代表其它区间 | Codex R55-B 只读评审（分区覆盖） |
| R56-A (Codex gpt-6.1-terra) | `plan-20261117-branch.md` 第 1–1000 行、完成判据与 `DEP-PM-06` | PASS | 无 P0/P1/P2。普通 `-m`/`-M` 配置迁移与恢复、`DEP-BR-09`、`REL-BR-03` 均已核对 | 与 R55-B 共同构成计划 Codex 单审通过 | Codex R56-A 只读评审（分区覆盖） |
| R53-A (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P1: 普通 `-m` 恢复遗漏源/目标 config。已把 `branch.<name>.*` 操作前快照与逐项恢复断言加入 AC 和故障注入测试。R54-A 分区复核 | R54-A 分区完整复审 | Codex R53-A 只读评审（分区覆盖） |
| R53-B (Codex gpt-6.1-terra) | `plan-20261117-branch.md` | FAIL | P2: 风险表遗漏 `BR-40`，且 `BR-25` 描述仍像重新新增解析器。已补风险关联卡并明确在 `BR-40` 入口上扩展。本轮未完成全分区复审，R54-B 不得据此推定其它维度通过 | R54-B 分区完整复审 | Codex R53-B 只读评审（部分覆盖） |

## 实施证据汇总

- 计划成稿时间：`2026-11-17 02:53:00 UTC`；最新修订时间：`2026-11-17 12:27:28 UTC`；仓库基线 `main@62abb622f0eb2626934633ada78061963ebf1f44`；`Cargo.toml` 版本 `0.30.207`。
- 后续各卡的 focused 测试计数、nextest 全量摘要、`release.yml` run ID 与 D 组结果将直接登记于本节。

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-BR-01 | 无（21 个分配 issue 已全量拆卡进入 `BR-01`..`BR-32`、`BR-34`、`BR-36`..`BR-40`；`BR-33` 与 `BR-35` 撤回） | N/A | N/A | N/A |
| DEFER-BR-02 | 外部完整上游 Git shell 脚本全量多用例跑通（`t3200`/`t3203`/`t3204`/`t5514` 整套 harness；注 `#866` 指定的 `t5514#9` `skipFetchAll` 行为已在 `BR-24` `command::fetch_test::skip_fetch_all` 完整覆盖并验收） | 整套外部脚本运行受外部前置 `DEP-BR-03`（`#686`）、`DEP-BR-04`（`#676`）、`DEP-BR-05`（`#541`）、`DEP-BR-06`（`#675`）、`DEP-BR-07`（`#556`）阻塞 | 上述前置合入后由 CLI / Git-compat 维护者执行上游回归门 | 外部上游兼容性回归计划（Owner: CLI / Git-compat 维护者） |

## 完成判据

计划只有在以下条件全部满足后才能标记完成：

- [ ] 所有任务卡满足粒度规则 `G-*`：无未登记的 L 例外、无 XL 卡、无碎片卡、无未登记的合并发布例外、实现写集冲突均已消解；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务的 acceptance criteria 已满足，且 `Lifecycle=done` **且** `Acceptance=complete`（ER-04）。
- [ ] 所有任务的 Verification 命令已运行并记录结果。
- [ ] **计划完成门 / 全量收口门（ER-13）**：在全部任务卡完成后运行一次 —— `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿。
- [ ] **全量收口门暴露的 Bug 已全部处理（ER-13）**。
- [ ] 必要的 docs/compat/error-code/test-index 更新已完成。
- [ ] 每张卡的 `Rollback mode` 均已验证或记录。
- [ ] Codex review 最终结论为 `PASS`，P0/P1 全部关闭（按用户 2026-11-17 指令，不再要求 Claude Review）。
- [ ] 独立实现卡均已完成 `patch + 1`、版本面三处同步、工具链刷新 `Cargo.lock`、`gh release create v<version>` 触发 `release.yml` 并取得 Cloudflare 发布证据（ER-08）。`REL-BR-03` 子卡 `BR-03`/`BR-37`/`BR-38` 不单独 bump 或推送，均继承 `BR-39` 的统一发布、C/D 与 Cloudflare 证据；`BR-39` 已完成相同版本与发布门。`BR-33` 与 `BR-35` 已撤回，不发布。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
- [ ] `plan-long.md` 相关状态或日期计划索引已同步，或明确 `N/A`。
