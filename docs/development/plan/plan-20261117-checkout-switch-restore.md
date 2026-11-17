# `libra checkout` / `switch` / `restore` Git 兼容与缺陷修复计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：Codex R44-A1、R45-A2、R48-B 分区 PASS，计划可提交；全部任务 `pending`，尚未执行。最近修订 `2026-11-17 12:27:28 UTC`。本计划不在 v2.16 `code-only` 具名范围内，执行标准 `patch + 1` + `gh release create` 发布合同。按用户 2026-11-17 指令，计划提交门为 Codex 单审，不再要求 Claude Review。
> 覆盖 Open Issues（20 个）：`#702`、`#703`、`#704`、`#705`、`#706`、`#707`、`#708`、`#709`、`#712`、`#713`、`#714`、`#715`、`#716`、`#718`、`#721`、`#723`、`#724`、`#1008`、`#1030`、`#1037`。

## 文档职责

本文解决 `libra checkout`、`libra switch`、`libra restore` 及关联 D/F 合并路径（`src/command/merge.rs`）与状态检查（`src/command/status.rs`）在 Git 兼容测试中的 20 项已知缺陷，目标是逐卡交付可独立验收、可独立发布、附带单元/集成测试与中英文/网站文档同步的修复切片。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收。

### 适用范围

- `libra restore` 与 `libra checkout` 的 `-p`/`--patch` 交互模式（复用 `src/internal/patch_mode/` 并移除 `src/cli.rs:3028-3048` D15 拒绝表，同步 `tests/compat/compat_declined_interactive_flags.rs` 与 `docs/development/commands/_compatibility.md`）。
- `libra checkout` 的 `--overlay`/`--no-overlay`、`-2`/`--ours`/`-3`/`--theirs`、`<tag>` 源树、`.` 路径消歧、`--detach`+`-b` 互斥、追踪状态、`-pathspec-from-file`、`--track=direct|inherit`、`-f` 符号链接安全、`-m` 合并切换、`status.rs` D/F 检查、前导目录符号链接、`--orphan <start-point>`、unborn `-b`、DWIM 多远端/单远端、`-B` 保留暂存与非空目录替换。
- `libra merge` 的 D/F 冲突阶段记录（`destdir~HEAD` stage 1/2，`src/command/merge.rs:4031`、`src/command/merge/rename_merge.rs`）。
- `libra switch` 在 unborn HEAD 下的 `-C <branch>` 与共享工作区/DWIM 逻辑（`src/command/switch.rs`）。
- 对应测试（`tests/command/{checkout_test,switch_test,restore_test,merge_test}.rs`，均编译在 `--test command_test`）、命令/开发/网站文档（`../libra-backend/apps/tanstack-app/content/docs/commands/`）、`COMPATIBILITY.md` 与 `docs/error-codes.md`。

### 非目标

- 不更改既有 `checkout --ignore-other-worktrees` 的有意设计差异（Libra 禁止同一分支在多个 worktree 同时检出）。
- 不在本计划中修改 `branch` 或 `grep` 命令自身缺口（由同批并行计划承接）。
- 不引入新的 SQLite schema 迁移或修改 `GlobalConfig` / `SystemConfig` 表结构。

### 成功定义

- 20 个指定 GitHub Issue 对应的全部行为缺口在代码中修复，并有确定性 focused 测试覆盖正常路径与失败路径。
- 每个修改用户可见行为的任务卡同步更新对应命令的 EN/zh-CN 文档、开发文档、网站文档及 `COMPATIBILITY.md`。
- 全部 33 张活动任务卡（`CSR-35` 已合回 `CSR-09`，编号保留为废弃记录；含 4 张家族发布点 `CSR-25`/`CSR-31`/`CSR-32`/`CSR-34`）达到 `Lifecycle=done` 且 `Acceptance=complete`，全部卡与收口全量门及 Codex review 均通过（按用户 2026-11-17 指令，不再要求 Claude Review）。

## 使用规则

- 每次状态推进、快照更新与「修订历史」新增行都记录实际 `YYYY-MM-DD HH:MM:SS UTC` 时钟时间（v2.11）。
- 新建本计划或推进任意任务卡时，必须在同一变更中同步更新 [`plan-status.md`](plan-status.md)（本计划批量建稿期由协调者在统一提交中完成 `plan-status.md` 登记以避免并发写锁冲突；执行期每卡推进须同批更新 `plan-status.md`）。
- 所有测试摘要、计数、SHA-256、run ID、C/D 组门结果均写入本计划「实施证据汇总」或对应任务卡记录，不得新建 `evidence/`、`baseline/`、`scratch/` 等独立证据目录或文件（v2.12）。
- 终止链末端如存在纯文档记账提交，其远端 CI 观察由下一次 `plan-status.md` 例行同步补记（v2.14）。

## 事实基线

> 基线检查时间：2026-11-17 02:50:03 UTC，仓库基线 `main` @ `62abb622f0eb2626934633ada78061963ebf1f44`（`Cargo.toml` 版本 `0.30.207`）。所有行号必须在开工当天刷新。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `checkout.rs`（1456 行）、`switch.rs`（1900 行）、`restore.rs`（2981 行）、`merge.rs`、`status.rs` | `src/command/checkout.rs:53-120`、`src/command/switch.rs:53`、`src/command/restore.rs:265` |
| 已有 Patch 引擎与 D15 拒绝表 | `src/internal/patch_mode/` 已驱动 `add -p`/`reset -p`；`restore -p` 与 `checkout -p` 在 `src/cli.rs:3028-3048` 被 D15 拒绝表拦截 | `src/internal/patch_mode/`、`src/cli.rs:3028-3048`、`tests/compat/compat_declined_interactive_flags.rs` |
| 关键分发与状态路径 | `disambiguate_path_argument`/`index_matches_path`、`resolve_checkout_create_startpoint`、`ensure_switch_clean_or_force`、`status.rs:745`、`merge.rs:4031` | `src/command/checkout.rs:664-704,960-969`、`src/command/switch.rs:852-867`、`src/command/status.rs:745`、`src/command/merge.rs:4031` |
| 用户命令 | `Commands::Checkout`、`Commands::Switch`、`Commands::Restore` 注册于 `src/cli.rs` | `src/cli.rs:624-627` |
| 机器输出与错误码 | `CheckoutOutput`、`SwitchOutput`、`RestoreOutput` 支持 `--json`；错误码契约位于 `docs/error-codes.md` | `src/command/checkout.rs:122-143,1229`、`docs/error-codes.md` |
| 文档 | `docs/commands/{checkout,switch,restore,merge}.md`（EN+zh-CN）、`docs/development/commands/{checkout,switch,restore,merge}.md`、`_compatibility.md`、网站 `../libra-backend/apps/tanstack-app/content/docs/commands/`、`COMPATIBILITY.md` | `COMPATIBILITY.md:190,273` |
| 测试 | 集成测试模块 `checkout_test`、`switch_test`、`restore_test`、`merge_test` 注册于 `tests/command/mod.rs`，由 `--test command_test` 运行 | `tests/command/mod.rs:1020,1124,1150` |
| 外部参照 | Git `2.56.0` @ `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（`t/t2012`、`t/t2020`、`t/t2024`、`t/t2025`、`t/t2060`、`t/t2070`、`t/t3404`、`t/t1008` 等）；核对日期 2026-11-17 | `gh issue view`（#702..#724, #1008, #1030, #1037） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-CSR-01 | `restore -p/--patch` 在 `src/cli.rs:3028-3048` 被 D15 拒绝，未接入 `src/internal/patch_mode/` | `#1037` 交互式 hunk 恢复不可用 | `src/cli.rs:3028-3048`、`src/internal/patch_mode/` | `CSR-01` |
| GAP-CSR-02 | `checkout -p/--patch` 在 `src/cli.rs:3040` 被 D15 拒绝，未复用 `patch_mode` | `#702`（部分 1/3）patch 检出不可用 | `src/cli.rs:3040`、`src/command/checkout.rs:53-120` | `CSR-02` |
| GAP-CSR-03 | `checkout` 委托 `restore` 默认采用 `--no-overlay`，缺少 `--overlay` 默认与 `-p --overlay` 互斥报错 | `#702`（部分 3/3）默认语义与冲突信息不兼容 | `src/command/checkout.rs:102-107,720-722` | `CSR-03` |
| GAP-CSR-04 | `CheckoutArgs` 未注册 `--theirs` / `--ours`（含 `--theirs --no-overlay` 删文件） | `#703` 无法在 `checkout` 选边解决冲突 | `src/command/checkout.rs:53-120` | `CSR-04` |
| GAP-CSR-05 | `checkout --no-overlay <tag> -- <path>` 未正确解析 tag 为源树 | `#704` tag 源检出失败 | `src/command/checkout.rs:706-757` | `CSR-05` |
| GAP-CSR-06 | `index_matches_path` 对 `.` 不匹配且未覆盖 `--no-progress`/`--quiet --progress` | `#705` `checkout --no-progress .` 误入切分支路径 | `src/command/checkout.rs:695-704` | `CSR-06` |
| GAP-CSR-07 | `CheckoutArgs` 允许 `--detach` 与 `-b`/`-B`/`--orphan` 联用 | `#706` 违反 Git 互斥契约 | `src/command/checkout.rs:57-77` | `CSR-07` |
| GAP-CSR-08 | `checkout` 切换分支后未打印 ahead/behind 追踪状态行（含本地 `.` 上游） | `#707` 缺少上游追踪提示 | `src/command/checkout.rs:1229-1286` | `CSR-08` |
| GAP-CSR-09 | `CheckoutArgs` 缺少 `--pathspec-from-file`/`--pathspec-file-nul` 及互斥报错（含 `--detach`） | `#708` 批量路径文件输入不可用 | `src/command/checkout.rs:53-120` | `CSR-09` |
| GAP-CSR-10 | `CheckoutArgs.track` 为 `bool`，不支持 `--track=inherit`/`direct` 与 `autoSetupMerge=inherit` | `#709` 追踪继承模式失效 | `src/command/checkout.rs:84-85,899-914` | `CSR-10` |
| GAP-CSR-11 | `checkout -f` / `switch -f` 删除条目时跟随符号链接误删目标目录内容 | `#712` 安全/数据丢失风险 | `src/command/restore.rs:2037-2054,2206` | `CSR-11` |
| GAP-CSR-12 | `CheckoutArgs` 拒绝 `-m`/`--merge`，缺少三方合并与后续 `checkout -f` 消解冲突 | `#713` 无法携带本地修改合并切换 | `src/command/checkout.rs:53-120` | `CSR-12` |
| GAP-CSR-13 | `status.rs:745` 在跟踪目录被文件替换后触发 `LBR-IO-001`；普通 `checkout HEAD` 应保留用户修改，仅 `--force HEAD` 恢复 | `#714` 状态预检误报错 | `src/command/status.rs:745`、`src/command/checkout.rs` | `CSR-13` |
| GAP-CSR-14 | `restore.rs:1254` 在 `--force` 下仍拒绝将前导目录符号链接替换为真实目录 | `#715` 强制切换无法替换目录链接 | `src/command/restore.rs:1254-1260` | `CSR-14` |
| GAP-CSR-15 | `run_checkout` 显式拒绝 `checkout --orphan <b > <start-point>` | `#716` 不支持 orphan 起点参数 | `src/command/checkout.rs:461-470` | `CSR-15` |
| GAP-CSR-16 | `resolve_checkout_create_startpoint` 在 unborn orphan 上 `checkout -b <new>` 无起点时失败 | `#718` unborn 下 `-b` 应成功切换 | `src/command/checkout.rs:960-969` | `CSR-16` |
| GAP-CSR-17 | 名称匹配多个远程分支时 DWIM 回退为 pathspec 检查，且缺 `matched multiple (N)` 文案 | `#721` 多远端歧义未正确报错 | `src/command/checkout.rs:664-694`、`src/command/switch.rs:179,655` | `CSR-17` |
| GAP-CSR-18 | `switch::ensure_switch_clean_or_force` 在 `checkout -B` 时拒绝保留已暂存可合并改动 | `#723` `-B` 预检过严 | `src/command/switch.rs:852-867`、`src/command/checkout.rs:977` | `CSR-18` |
| GAP-CSR-19 | `checkout <branch>` 的 `get_remote` 硬编码 `origin/`，未从唯一非 `origin` 远程追踪引用 DWIM | `#724` 单一远端 DWIM 失效 | `src/command/checkout.rs:1002-1022` | `CSR-19` |
| GAP-CSR-20 | `checkout` / `switch` 拒绝用跟踪文件替换非空工作区目录 | `#1008`（部分 1/3）非空目录 D/F 切换失败 | `src/command/checkout.rs`、`src/command/restore.rs:1601` | `CSR-20` |
| GAP-CSR-21 | `merge --strategy=recursive` 处理 D/F 冲突时 `destdir~HEAD` 阶段记录与 `modify/delete` 消息与 Git 不一致 | `#1008`（部分 3/3）`destdir~HEAD` 应为 stage 1 与 2 | `src/command/merge.rs:4031`、`src/command/merge/rename_merge.rs` | `CSR-21` |
| GAP-CSR-22 | `libra switch -C <branch>` 在 unborn HEAD 上失败 | `#1030` `t3404` 初始化失败 | `src/command/switch.rs:980` | `CSR-22` |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-long.md` | 长期路线参考 | 不触碰长期路线状态；本计划聚焦 20 个已归档缺陷收敛 |
| 同批并行计划（`branch` / `commit-signing` / `merge-pull` / `worktree-clean-reset-apply`） | 共享文件串行依赖 | 通过 `DEP-CSR-03`..`06` 严格串行化 `src/cli.rs`、`src/command/merge.rs`、`src/command/restore.rs`、`src/command/branch.rs` 写集（G-10） |
| `plan-status.md` | 全仓计划单一视图 | 本计划建稿由协调者统一登记；执行期每卡推进同批更新 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 20 个 `checkout`/`switch`/`restore`/`merge` 兼容缺陷覆盖 patch、合并检出、DWIM、D/F 冲突与符号链接安全，值得逐轴收敛 | 无 |
| 可行性 | 落点集中在 `checkout.rs`、`switch.rs`、`restore.rs`、`status.rs`、`merge.rs`、`cli.rs` | 拆分为 33 张活动小卡（含 4 张家族发布点 `CSR-25`/`31`/`32`/`34`；`CSR-35` 已合回 `CSR-09`） |
| 任务卡粒度 | `G-03` 按独立谓词计数；仅 `CSR-09` 因同一行为轴的互斥门族登记 `EX-CSR-03`（`15/8@EX-CSR-03`，`VER=15/8@EX-CSR-03`），其余活动实现卡 AC≤8、发布点 ≤12，无其它 `EX-*` | 全部活动卡满足 `M` 或带白名单 `@EX-CSR-03` |
| 依赖与顺序 | 共享写集严格串行 `CSR-01 -> ... -> CSR-22` | 无并发 I–I 冲突 |
| 完整性 | 修正 `CSR-13`（保留修改 + `status.rs:745`）、`CSR-01`/`02`（复用 `internal/patch_mode` + `cli.rs` D15）、`CSR-21`（`merge.rs` stage 1/2）、`CSR-16`（unborn `-b` 成功）、`CSR-03`（overlay 默认）、`CSR-06`（`.` + `--no-progress`）、`CSR-09`（`--detach` + 共享写集 `T-1`）、`CSR-18`（`switch.rs:852` `T-1`） | 无遗漏 |
| 安全性 | `CSR-11`/`CSR-14` 严格使用 `symlink_metadata`；`CSR-13` 普通 `checkout HEAD` 绝不删除用户替换文件 | 在对应卡明确 |
| 功能正确性 | 精确对齐 Git `6de20f6092` 语义、stage 1/2、错误文案与退出码 | 已写入每卡 AC |
| 接口兼容 | `Rollback mode` 统一为 `immutable-release` 并附降级指引与兼容窗口 | 已登记 |
| 数据流与控制流 | `CSR-01`/`02` 复用既有 `src/internal/patch_mode/`（GC-02） | 无重复引擎 |
| 性能与容量 | 继承 GC-10 | 无无界扫描 |
| 可靠性与容错 | 禁止新增裸 `unwrap()`/`expect()` | 继承 GC-11 |
| 可维护性 | 公共路径/合并/patch 逻辑收敛在既有共享模块 | 已分层 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 03:03:00 UTC | 初稿建档 | 基于 `agent_4_tasks.json` 20 个 `checkout`/`switch`/`restore` issue 建立 `CSR-01`..`CSR-22` | N/A | 全文 |
| 2026-11-17 03:12:00 UTC | Claude & Codex R1 FAIL | 修正 `CSR-13`（#714 保留修改 + `status.rs:745`）；修正 `CSR-01`/`02` 复用 `internal/patch_mode/` 与 `cli.rs` D15；修正 `CSR-21` 为 `merge.rs` stage 1/2；修正 `CSR-16` unborn `-b` 成功；修正 `CSR-03` overlay 默认与报错；修正 `CSR-06`/`09`/`12`/`18` 遗漏场景与写集 `T-1`；统一 `immutable-release` + D 组门 + 原子 AC 计数 | 无拆卡 | 实施顺序、粒度审计表、追溯表、review log |
| 2026-11-17 03:20:00 UTC | Claude & Codex R2 FAIL | 修正 `base.yml` job 与 `cancel-superseded-main.yml` 口径；登记 `DEP-CSR-03..06`；`CSR-01` 拆出 `CSR-23`、`CSR-09` 拆出 `CSR-24` 以严格满足 G-03 原子计数；`CSR-02` 补齐 `--no-auto-advance` D15 解禁与 `README.md:97`；`CSR-11` 锚定 `restore.rs:2206`；`CSR-12` 补 `switch.rs`/`restore.rs` `T-1` 与 `-f -m` 128 互斥；`CSR-05` 补 `*file3`、`CSR-08` 补 stderr、`CSR-13` 补 HEAD 留分支断言 | `CSR-01` → `CSR-01,23`；`CSR-09` → `CSR-09,24` | 全文 |
| 2026-11-17 03:27:30 UTC | Codex R3 FAIL | 将 `CSR-01`+`CSR-23` 设为家族卡 `REL-CSR-01`（发布点 `CSR-25`），`CSR-09`+`CSR-24` 设为家族卡 `REL-CSR-02`（发布点 `CSR-26`），确保引入选项与完整互斥校验同版发布；修正 `CSR-01`/`09` `Granularity` AC 计数 | 新增 `CSR-25`, `CSR-26` | 发布分组、实施顺序、审计表、追溯表、review log |
| 2026-11-17 03:33:00 UTC | Claude & Codex R4 FAIL | 补齐 `DEFER-CSR-02`；`CSR-01`/`02`/`10`/`21` 接入 `DEP-CSR-03..06`；`CSR-01`/`09` `Release write set` 改 `N/A`；`CSR-01` 补 `--source=HEAD`/`HEAD^` AC；`CSR-21` AC2 限定 `#1008` `destdir~HEAD` 场景避免与 `MP-17` 冲突；`CSR-25`/`26` 拆为 6 条原子 AC（`6/12`）；对齐 `CSR-11`/`12`/`23`/`24` 锚点与计数；同步 M1/测试矩阵/完成判据与时间正序修订历史 | N/A | 任务卡、依赖表、延后表、测试矩阵、追溯表、里程碑、review log |
| 2026-11-17 03:40:00 UTC | Codex R5 FAIL | 将 `CSR-25`/`CSR-26` 拆为 9 条独立原子 AC（`9/12`，按 G-03 排除 ER-04 强制门 `fmt`/`clippy`/`T-2 nextest`/`compat_version_surface_sync`） | N/A | 审计表、`CSR-25`、`CSR-26`、review log |
| 2026-11-17 03:48:00 UTC | Claude & Codex R6 FAIL | 恢复 `CSR-05`/`06` `Release write set: Inherited`；`CSR-25`/`26` 严格按 G-03 仅计卡特有判据（`AC=4/12; VER=1/12`，排除 ER-04/ER-08 强制门）；`CSR-13` 补齐 `F.t` 保留/恢复；拆细 `CSR-02`/`03`/`08`/`10`/`19` 复合 AC；补齐 `DEP-CSR-03..05` 全同批协调口径、`CSR-07`/`02` exit 128、`#707` pull 提示、`#716` root commit、`#1037` `-p --source=HEAD^ -- dir`、`CSR-04`/`23`/家族子卡 `Full-suite trigger: N/A`、风险表 `T-1` 列表与 `DEFER-CSR-02` 承接位置 | N/A | 全文 |
| 2026-11-17 03:56:00 UTC | Codex R7 FAIL | 按 G-03 逐项拆细 `CSR-02`（`7/8`）、`CSR-08`（`8/8`）、`CSR-10`（`8/8`）与 `CSR-25`/`26`（`5/12`，分离 release note 与文档交付证据）的原子判据（Claude R7 遇到 `ECONNRESET` 网络中断） | N/A | 审计表、`CSR-02`、`CSR-08`、`CSR-10`、`CSR-25`、`CSR-26`、review log |
| 2026-11-17 04:02:00 UTC | Claude & Codex R8 FAIL | 按 G-03 严格拆分 `CSR-02`→`CSR-02,27`、`CSR-08`→`CSR-08,28`、`CSR-13`→`CSR-13,29`、`CSR-20`→`CSR-20,30`（含 `--auto-advance` 与各互斥/状态/D/F 原子判据）；`CSR-01` 同步 `6/8`；`CSR-03` 去「等」字；`CSR-25`/`26` 补 `libra log -n 2` 验证；全部 `restore.rs` 卡片接入 `DEP-CSR-05`；更新 `cli.rs:5329` | `CSR-02`→`02,27`；`CSR-08`→`08,28`；`CSR-13`→`13,29`；`CSR-20`→`20,30` | 全文 |
| 2026-11-17 04:08:00 UTC | Codex R9 FAIL | 将 `CSR-02`+`CSR-27`+`CSR-03` 设为家族 `REL-CSR-04`（发布点 `CSR-32`），将 `CSR-08`+`CSR-28` 设为家族 `REL-CSR-03`（发布点 `CSR-31`），消除中间版本缺 `-p --overlay` 互斥与静默/JSON 污染；`CSR-29` 将 `switch -f` 拆为 3 条原子 AC（`6/8`）（Claude R9 遇到 `ECONNRESET` 网络中断） | 新增 `CSR-31`, `CSR-32` | 发布分组、实施顺序、审计表、`CSR-02`/`03`/`08`/`27`/`28`/`29`、`CSR-31`/`32`、追溯表、里程碑、review log |
| 2026-11-17 04:14:00 UTC | Claude & Codex R10 FAIL | `CSR-11` 拆出 `CSR-33`（`6/8` + `3/8`）；拆细 `CSR-04`（`6/8`）与 `CSR-14`（`5/8`）并对齐 `CSR-14` `Granularity` `deps=CSR-29`；`CSR-29` `switch -f` 指定 `<target-branch>`；精确 `cli.rs` 行号（`3029`/`3040`/`3051`/`3062`）并将 `cli.rs:5320` 移至 `CSR-27`；`CSR-17` 补 `switch_test` 验证；同步 `CSR-13` 审计 `3/3` 与追溯表、`#702`/`#1008` 部分编号、Phase 1/2 标题与退出、成功定义 `33` 卡、风险表与完成判据 `CSR-31`/`32` | `CSR-11` → `CSR-11,33` | 全文 |
| 2026-11-17 04:22:00 UTC | Codex R11 FAIL（Claude R11 `ECONNRESET`） | 修正 `CSR-29` 正文 3 条 `switch -f` 判据占位符为 `<target-branch>`（与 `switch.rs:1554` 区分当前分支 no-op）；将 `#712` 重拆为按命令独立行为轴的 `CSR-11`（`checkout` 符号链接安全 + 同卡常规文件/无关修改/索引回归，`6/8`）与 `CSR-33`（`switch` 符号链接安全 + 同卡常规文件/无关修改/索引回归，`6/8`），消除变动与回归测试分离（G-02） | N/A | 审计表、`CSR-11`、`CSR-33`、`CSR-29`、追溯表、实施证据汇总、review log |
| 2026-11-17 04:34:00 UTC | Claude & Codex R12 FAIL | 将 `CSR-33` 合并回 `CSR-11`（`8/8` 原子 AC，`3/8` VER），同卡覆盖 `checkout -f HEAD~`（`switch::checkout_to_commit` `switch.rs:1405,1433`）与 `switch -f <target-branch>`（`move_to_resolved_branch` `switch.rs:1607,1691`），修正 `#712` 实测 `mv dir untracked && ln -s untracked dir` 场景、移除无 Git 依据的普通 checkout 拒绝 AC、明确 `-f` 保留无关未跟踪文件并丢弃已暂存/已跟踪改动使索引与目标树一致；修正 `CSR-11` 根因锚点至 `restore.rs:2206`；在 `CSR-14`/`29`/`30` 补齐 `restore.rs:1941-1997`（`preflight_worktree_restore_to_commit`）及 `switch.rs:1433,1607` 锚点；统一全文至 `32` 卡并修正 `Codex review log` R11 评审者记录 | `CSR-11,33` → `CSR-11` | 全文 |
| 2026-11-17 04:43:00 UTC | Claude & Codex R13 FAIL | 修正 `CSR-12` 残留依赖 `CSR-33` → `CSR-11`；将 `CSR-23` 合回 `CSR-01`（取消 `REL-CSR-01`/`CSR-25`）、将 `CSR-24` 合回 `CSR-09`（`8/8`，取消 `REL-CSR-02`/`CSR-26`）以消除 G-02 实现与互斥校验拆分；按 `plan-template.md` 门族型验收白名单为 `CSR-11`（`10/8@EX-CSR-01`）与 `CSR-01`（`9/8@EX-CSR-02`）登记 waiver 与「判据规范」块；修正 `CSR-11` 不跟随符号链接断言（不强绑必须删除或保留符号链接本身）、`switch.rs:1412`/`1540`、`checkout.rs:1161`、`restore.rs:1941-1997` 精确行号与全文 `28` 卡计数 | `CSR-01,23,25` → `CSR-01`；`CSR-09,24,26` → `CSR-09` | 全文 |
| 2026-11-17 04:56:00 UTC | Codex R14 FAIL（Claude R14 `ECONNRESET`） | 移除 `EX-CSR-01`/`EX-CSR-02` waiver：`CSR-01` 移除冗余 D15 守卫 AC9 收敛至 `8/8`；`CSR-09` 拆出 `CSR-24`（`-p`/`-b`/`-B`/`--orphan` 互斥，`4/8`）使 `CSR-09` 自身含基础读取与核心互斥（`7/8`）；`CSR-11` 拆出 `CSR-33`（`switch -f`，`5/8`）并设家族 `REL-CSR-05`（发布点 `CSR-34`）同版发布双命令校验；登记 `DEP-CSR-07` 串行化跨计划共享 `COMPATIBILITY.md`/`docs/error-codes.md`；统一全文至 `31` 卡 | `CSR-09` → `CSR-09,24`；`CSR-11` → `CSR-11,33,34` | 全文 |
| 2026-11-17 05:09:00 UTC | Codex R15 FAIL | `CSR-09` 纳入全部 7 项 `--pathspec-from-file` 互斥与缺失文件诊断（`8/8`），`CSR-24` 改为独立行为轴 `--pathspec-file-nul`（`3/8`），确保 `CSR-09` 独立发布时不缺任何互斥校验；`CSR-33` 合回 `CSR-11` 并取消 `REL-CSR-05`/`CSR-34`，按 `plan-template.md:572` 为 `CSR-11` 登记 `EX-CSR-01`（`10/8@EX-CSR-01`，10 个独立具名可执行 `nextest` 门禁命令，`Approver: Eli (Plan Owner)`）；`DEP-CSR-07` 明确协调者批次串行调度门控；统一全文至 `29` 卡与 `../libra-backend` | `CSR-11,33,34` → `CSR-11` | 全文 |
| 2026-11-17 05:12:00 UTC | Codex R16 FAIL（Claude R16 `ECONNRESET`） | 取消 `EX-CSR-01` waiver 并拆卡消除全部复合谓词：`CSR-01` 拆为 `CSR-01` + `CSR-23` 经 `REL-CSR-01` 发布点 `CSR-25` 联合发布；`CSR-09` 拆为 `CSR-09` + `CSR-33`；`CSR-11` 拆为 `CSR-11` + `CSR-35` 经 `REL-CSR-06` 发布点 `CSR-36` 联合发布；`CSR-31`/`CSR-32` 补齐所属家族全部新守卫命令；将 D 组默认显式扩展至 `family release point`（含 artifact/CDN 可用性）；同步 `CSR-05`/`06` `Granularity` `deps=` | `CSR-01`→`01,23,25`；`CSR-09`→`09,33`；`CSR-11`→`11,35,36` | 全文 |
| 2026-11-17 05:17:00 UTC | Codex R17 FAIL | 修正 `CSR-01` 在首卡即解禁 `src/cli.rs` D15 并跑 `compat_declined_interactive_flags`，并将 `y`/`n`/`q` 拆为独立单谓词 AC（`CSR-01` `7/8` + `CSR-23` `8/8`）；重划 `CSR-09`（`--pathspec-from-file=<file>` + 缺失 128 + 全部 6 项互斥，`8/8`）与 `CSR-33`（`--pathspec-from-file=-` stdin，`2/8`，取消多余的 `REL-CSR-05`/`CSR-34`）；将 `CSR-11`/`CSR-35` 由按命令拆分改为按行为轴拆分（`CSR-11` `4/8` 同卡覆盖 `checkout -f` + `switch -f` 符号链接不跟随，`CSR-35` `6/8` 同卡覆盖双命令常规恢复/未跟踪保留/索引同步，经 `REL-CSR-06` `CSR-36` 联合发布）；修正 `M1` 计数为 7 独立 + 3 家族发布点，全量同步 `34` 卡 | 移除 `CSR-34`/`REL-CSR-05` | 全文 |
| 2026-11-17 05:37:00 UTC | Codex R18 FAIL（Claude R18 `ECONNRESET`） | 将 `CSR-35` 回归覆盖并回 `CSR-11` 并移除 `REL-CSR-06`/`CSR-36`（消除 G-02 纯回归测试卡）：`CSR-11`（`8/8`，`independent`）在 `restore.rs:2206` 实施**逐路径组件（component-wise）** `symlink_metadata` 前导符号链接检查（绝不对 `dir/f` 整串检查后穿过前导符号链接删除外部目标）并同卡验证 `checkout -f` 与 `switch -f` 各自退出 0、保护目标目录、写入常规文件及保留未跟踪/索引一致；将网站文档路径统一改为本地已挂载的 `../libra-backend`（`DEP-CSR-02` 保留 `../libra-backend` 挂载优先核对规则）；全量同步 `32` 卡 | 移除 `CSR-35`/`36`/`REL-CSR-06` | 全文 |
| 2026-11-17 05:45:00 UTC | Codex R19 FAIL（Claude R19 `ECONNRESET`） | 移除 `发布分组与并发窗口` 残留 `REL-CSR-06`/`CSR-35`/`CSR-36`，`DEP-CSR-02` 补充 `/Volumes/Sea/libra-backend` 挂载优先核对，`CSR-11` 将未跟踪保留与索引同步拆为独立谓词（`10/8@EX-CSR-01`） | 移除 `CSR-35`/`36`/`REL-CSR-06` | 依赖表、发布分组、`EX-CSR-01`、`CSR-11`、审计表、review log |
| 2026-11-17 06:58:00 UTC | Codex R20 FAIL（Claude R20 `ECONNRESET`） | 正式恢复登记 `EX-CSR-01`（R21，依 `plan-template.md:572` 门族型验收白名单）；在 `DEP-CSR-02` 明确 `../libra-backend` 与 `/Volumes/Sea/libra-backend` 身份等价/remote/`cf` 分支核对与 STOP 条件并接入全部 29 张实现卡；全部 29 张实现卡补齐同卡 ER-06/ER-06a 逐文件文档验收门（不计入 G-03 卡特有 AC 分子）；拆细 `CSR-01`/`05`/`06`/`07`/`12`/`14`/`15`/`16`/`17`/`19`/`20`/`21` 复合 AC 至严格单谓词并全量同步 `Granularity` 与审计表；ER-12 具名发布者为 `Eli` 指定的单一串行发布者 | N/A | 全文 |
| 2026-11-17 07:08:00 UTC | Claude & Codex R21 FAIL | 移除 `EX-CSR-01` 并将 `CSR-11` 收敛至 `8/8`；新建 `REL-CSR-05`（`CSR-09`+`CSR-35`+`CSR-33`+`CSR-24` 经 `CSR-34` 联合发布）消除 `CSR-09` 15/8 超限与 `CSR-33` 独立发布割裂；拆细 `CSR-03`/`18`/`20`/`22`/`24`/`30` 复合 AC 并同步 `Granularity` 与审计表；修正 `CSR-25` `VER=5/12`、`split-from`、`CSR-21` AC5、`compat_declined_interactive_flags.rs` 锚点、`CSR-02` 范围说明、具名发布者 `Eli` 与家族 `cf` 推送时序（全计划 34 卡） | `CSR-09`→`CSR-09,35,34`；移除 `EX-CSR-01` | 全文 |
| 2026-11-17 07:46:33 UTC | Claude & Codex R22 FAIL | 将 `CSR-35` 合回 `CSR-09` 并登记 `EX-CSR-03`；同步 33 张活动卡与 4 个家族发布点；补追溯表、`CSR-11` 双入口回归、家族 `cf` 推送 AC，并拆开 `CSR-12`/`14`/`21`/`33` 复合谓词 | `CSR-35` → `CSR-09` | 评审结论、waiver、审计表、REL-CSR-05、追溯表、里程碑、Review log |
| 2026-11-17 08:28:29 UTC | Plan Owner Eli 书面批准 | 批准 `EX-CSR-03`：`CSR-09` 的 15 个互斥门留在同一恢复轴。`CSR-33` 自 `CSR-09` 拆出标准输入轴 | `CSR-09` → `CSR-09, CSR-33` | waiver、`CSR-09`、`CSR-33`、Review log |
| 2026-11-17 09:21:13 UTC | Codex R27 FAIL | 全局执行规则区分独立卡、家族子卡与发布点：子卡不 bump、不推送、不单独关闭 C/D；D 组默认门列明 `base.yml`/`codeql.yml` `analyze` 矩阵与 `release.yml` 五个 job；`CSR-33`/`CSR-24` 的文档 AC 覆盖清单内网站文件的本地更新，`cf` 推送仍由 `CSR-34` 承载 | N/A | 执行检查、D 组默认门、`CSR-33`、`CSR-24`、Review log |
| 2026-11-17 10:43:14 UTC | Codex R29 FAIL | D 组把 PR head 与 `main` push 拆开取证：PR 记录 PR head SHA、PR ref 和目标分支 `main`；push 记录 squash merge SHA 与 ref `main`；两边用 PR 合并记录关联，不再要求同一个 SHA。`codeql.yml` 使用同一口径 | N/A | D 组默认门、Review log |
| 2026-11-17 10:52:35 UTC | Codex R30 FAIL | `CSR-11` 的 8 条具名测试改为计入 `VER=8/8`。`CSR-13` 补 `status` 四份文档，并写明不改 `docs/error-codes.md`。缩写的文档影响字段展开为逐文件路径 | N/A | `CSR-11`、`CSR-13`、各卡文档影响、Review log |
| 2026-11-17 11:14:03 UTC | Codex R31 FAIL | 追溯表把 `_compatibility.md` 改到 `CSR-01`。`CSR-13` 追溯行补上 `status` 文档 | N/A | 追溯表 |
| 2026-11-17 11:19:54 UTC | Codex R32 FAIL | D 组补上 `download.libra.tools` 四个平台产物和 `install.sh` 的 HTTP 200 检查 | N/A | D 组默认门 |
| 2026-11-17 12:00:07 UTC | Codex R35 FAIL | D 组补入 Windows 签名清单使用的无后缀 `libra-windows-amd64`，并将 HEAD 检查改为五个二进制的完整 GET + `.size`/`.sha256` 核验及安装脚本下载检查 | N/A | D 组默认门、Review log |
| 2026-11-17 12:27:28 UTC | Codex R36 FAIL | M0 由 Claude+Codex 双审改为 Codex 单审，并复核现行门槛统一为用户指定的 Codex 单审 | N/A | M0、Review log |
| 2026-11-17 12:27:28 UTC | Codex R37 部分检查 | R37 因合并输出截断未完成，无新增 P1；同步头部与实施证据时间，下一轮按任务卡区间复审 | N/A | 头部、实施证据、Review log |
| 2026-11-17 12:27:28 UTC | Codex R38-A FAIL/P2 | CDN 改用已发布签名 manifest 的 `size`/`sha256`，不再要求不存在的 `.size` sidecar；`CSR-33` 证据改为复用 `restore.rs:383` 现有 stdin 读取器；跨计划写集补齐 `CSR-23`/`CSR-24` | N/A | D 组默认门、`CSR-33`、`DEP-WC-04`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R39-A FAIL | manifest 仅含四平台条目，Windows 仅无后缀 URL；改为无后缀对象先匹配 Windows 条目，`.exe` 仅与该条目比较 size/SHA，不要求第五条记录 | N/A | D 组默认门、Review log |
| 2026-11-17 12:27:28 UTC | Codex R40-A FAIL | `CSR-27` 增加不同 `-U` 与 inter-hunk 对 hunk 边界的影响，以及 `--auto-advance`/`--no-auto-advance` 对交互推进影响的具名断言，AC 从 5 调整为 7 | N/A | `CSR-27`、审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R41-A FAIL | `CSR-27` 固定第 5/15 行修改，精确断言 `-U=2` 的 2 行上下文与 5 行间隔、inter-hunk 5 合并/4 分裂、`-U=0` 无上下文，以及只应用第 5 行 hunk 后保留第 15 行修改 | N/A | `CSR-27`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R43-A1 P2 | `-U=2` 改为 Git 兼容短选项 `-U2`，`-U=0` 改为 `-U0`，并以目标 Git 版本验证 `checkout -p` 参数解析 | N/A | `CSR-27`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R44-A1 PASS | 第 1–500 行、CSR-27 及定向 CDN/CSR-33/DEP-WC-04 核对无 P0/P1/P2；`-U2`/`-U0`、固定 hunk 与最终文件断言、manifest 双 Windows 对象均通过复审 | N/A | 头部、Review log |
| 2026-11-17 12:27:28 UTC | Codex R44-A2 FAIL | `CSR-09` 将 `-B` 的两个测试由大写 `B` 的 `pathspec_from_file_B_*` 改为 snake_case 的 `pathspec_from_file_force_branch_*`，消除严格 Clippy `non_snake_case` 风险 | N/A | `CSR-09`、判据规范、Review log |
| 2026-11-17 12:27:28 UTC | Codex R45-A2 PASS | 第 501–1000 行及 CSR-27/CSR-11/CDN manifest 定向复核无 P0/P1/P2；`force_branch` 测试名与 Verification/判据命令一致 | N/A | 头部、Review log |
| 2026-11-17 12:27:28 UTC | Codex R46-B FAIL/P1 | `CSR-12` 增加合并阶段故障注入回滚 AC 与具名测试，合并 `-f/-m` 同一失败场景诊断判据以保持 8 条原子场景；`CSR-18` 增加 checkout -b/switch -C 共享预检冲突状态回归 | N/A | `CSR-12`、`CSR-18`、粒度审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R47-B FAIL/P2 | `CSR-18` 写集补入 `tests/command/switch_test.rs`；追溯表补入 `merge_checkout_failure_rolls_back` 与 `checkout_b_and_switch_c_conflict_preserves_state` | N/A | `CSR-18`、追溯表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R48-B PASS | 第 1001–1781 行无 P0/P1/P2；CSR-18 写集、CSR-12/CSR-18 追溯、故障回滚与共享预检回归均通过复审 | N/A | 头部、Review log |

## 已决议设计决策

### ADR-CSR-01: 复用既有 `src/internal/patch_mode/` 并解禁 `src/cli.rs` D15 标志

- **Status:** Accepted
- **Context:** `src/internal/patch_mode/` 已驱动 `add -p` 与 `reset -p`，而 `restore -p` 与 `checkout -p` 在 `src/cli.rs:3028-3048` 被 D15 拒绝表拦截。
- **Decision:** `CSR-01` 与 `CSR-02` 复用 `src/internal/patch_mode/`，解除 `src/cli.rs` D15 拦截并同步 `tests/compat/compat_declined_interactive_flags.rs` 与 `docs/development/commands/_compatibility.md`。
- **Alternatives considered:** 在 `restore.rs` 新建第二套 hunk 引擎。拒绝理由：违反 GC-02 单一事实源。
- **Consequences:** 修改 `src/cli.rs` 命中 ER-13 `T-1`。`CSR-01`/`CSR-02` 是家族子卡，`Full-suite trigger=N/A`，全量由发布点 `CSR-25`/`CSR-32` 的 `T-2` 执行。
- **Revisit when:** 仅当 `patch_mode` 架构重构时重审。

### ADR-CSR-02: `checkout` 默认恢复模式对齐 Git `overlay`

- **Status:** Accepted
- **Context:** Git `checkout <tree> -- <path>` 默认采用 overlay 模式（不删除源树中缺失的已跟踪文件），而当前 `checkout.rs:720-722` 委托 `restore` 采用了 `--no-overlay`。
- **Decision:** `CSR-03` 将 `checkout` 默认路径恢复对齐 Git 的 overlay 语义，保留 `--no-overlay` 显式开关，并支持 `--overlay` 互为覆盖。
- **Alternatives considered:** 维持默认 `--no-overlay` 并记为有意差异。拒绝理由：#702 明确要求与 Git 一致。
- **Consequences:** 显式需要删除缺失路径的调用方使用 `--no-overlay` 或 `libra restore`。
- **Revisit when:** 无需重审。

## 全局工程约束

继承 `plan-template.md` 全部 `GC-01`..`GC-13`。补充本计划专项约束：
- **GC-CSR-01:** 凡修改用户可见命令行为的卡，必须在同一卡更新对应命令 `docs/commands/<cmd>.md`（EN+zh-CN）、`docs/development/commands/<cmd>.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`（按 ER-06a 核对 VCS 与 `cf` 分支；若本地无对应有效 `cf` 仓库则 STOP 报告）及 `COMPATIBILITY.md`。
- **GC-CSR-02:** 涉及工作区文件/目录/符号链接删除或覆盖时，必须使用 `std::fs::symlink_metadata` 判别类型，严禁跟随符号链接删除或写入目标目录（GC-07）。

## 执行检查必备需求（强制）

继承 `plan-template.md` 全部 `ER-01`..`ER-14`。
- 每张卡开工前执行 `ER-01`、`ER-02`（刷新行号锚点）与 `ER-03`（复核 `G-*`）。
- 独立卡（`Release boundary: independent`）完成后执行 `ER-04`、`ER-05`、`ER-07`、`ER-08`（`patch + 1`、三处版本面同步、工具链刷新 `Cargo.lock`、`gh release create`）与 `ER-13`/`ER-14`，并完成 C/D 组。
- 家族子卡（`family child`）只完成本地 A/B、`ER-05` 与该卡写明的 `fmt`/`clippy`/focused 测试。子卡不 bump、不推送、不执行 `gh release create`，也不单独关闭 C/D。其 C/D 由该卡 `C/D coverage from` 指向的家族发布点承载（G-08）。
- 家族发布点（`family release point`）在所属子卡全部本地完成之后，才执行 `ER-08` 与 C/D 组。
- 全部 33 张活动卡完成后执行计划收口全量门：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

## 实施顺序

按 G-10 串行化写集，形成与 Phase 0/1/2 一致的无环顺序链：
- `CSR-01 -> CSR-23 -> CSR-25 -> CSR-02 -> CSR-27 -> CSR-03 -> CSR-32 -> CSR-04 -> CSR-05 -> CSR-06 -> CSR-07 -> CSR-08 -> CSR-28 -> CSR-31 -> CSR-09 -> CSR-33 -> CSR-24 -> CSR-34 -> CSR-10 -> CSR-11 -> CSR-12 -> CSR-13 -> CSR-29 -> CSR-14 -> CSR-15 -> CSR-16 -> CSR-17 -> CSR-18 -> CSR-19 -> CSR-20 -> CSR-30 -> CSR-21 -> CSR-22`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-CSR-01 | incoming | 上游基线 | `main@62abb622f0eb2626934633ada78061963ebf1f44` | 仓库维护者 | 干净可编译基线且 `compat_version_surface_sync` 通过 | `Cargo.toml:3` | 若版本面不一致，先建修复卡对齐再开工 |
| DEP-CSR-02 | incoming | 相邻文档仓库 | `../libra-backend`（本机工作区已就绪，`cf` 分支；若执行机存在 `/Volumes/Sea/libra-backend`，必须先核对两者指向同一 `libra-backend` remote/repository 身份且均在有效 `cf` 分支后再按唯一写集路径 `../libra-backend` 写入或同步） | 文档发布者 | 按 ER-06a 核对 VCS 类型、remote 身份、`cf` 分支及与 `/Volumes/Sea/libra-backend`（若挂载）的一致性 | `ER-06a` 检查命令 | 若无有效 `cf` 仓库、分支不对、或两路径仓库身份不一致则 STOP 报告，不强行覆盖 |
| DEP-CSR-03 | incoming | 同批并行计划 | 全部修改 `src/cli.rs` 的同批并行计划（含 `commit-signing` 等） | 协调者 | `src/cli.rs` 改动由协调者统一串行合并 | `plan-status.md` | 禁止并发修改 `src/cli.rs`，必须按顺序先后合并 |
| DEP-CSR-04 | incoming | 同批并行计划 | `plan-20261117-merge-pull.md`（`MP-17`..`MP-25` 共享 `merge.rs`/`rename_merge.rs`） | 协调者 | `merge-pull` 共享窗口完成后 `CSR-21` 基于最新 `main` 调整 `destdir~HEAD` 逻辑 | `plan-20261117-merge-pull.md` | `CSR-21` 在 `MP-17`..`MP-25` 对应窗口之后执行 |
| DEP-CSR-05 | incoming | 同批并行计划 | `plan-20261117-worktree-clean-reset-apply.md`（共享 `src/command/restore.rs`） | 协调者 | 双方对 `restore.rs` 改动按串行合并窗口错开（串行门控自 `CSR-01` 起生效） | `plan-20261117-worktree-clean-reset-apply.md` | 禁止并发修改 `restore.rs` |
| DEP-CSR-06 | incoming | 同批并行计划 | `plan-20261117-branch.md`（共享 `src/command/branch.rs`） | 协调者 | `branch.rs` 追踪逻辑改动串行合并 | `plan-20261117-branch.md` | `CSR-10` 与 `branch` 计划错开合并窗口 |
| DEP-CSR-07 | incoming | 跨计划共享文档写集 | `COMPATIBILITY.md`、`docs/error-codes.md`（与并行计划 `branch`/`grep`/`diff` 共享） | 仓库协调者 | 由协调者在 `plan-status.md` 对同批全部并行计划统一执行串行调度门控：独立卡每卡编辑共享文件前须取得协调者书面单写者放行，合入后立即释放；家族卡（`REL-CSR-01`/`03`/`04`/`05`）在首张子卡开工前取得**家族级锁**并沿同家族子卡内部顺序传递，直至家族发布点合入 `main` 后统一释放（窗口期禁止其它计划插入）；`COMPATIBILITY.md` 仅原地改 `checkout`/`switch`/`restore`/`merge` 行，`docs/error-codes.md` 允许相关卡复用/追加错码行 | 协调者批次串行调度表与逐卡/逐家族放行记录（合入后同步 `plan-status.md`） | 未获协调者当卡/当家族串行放行前阻塞开工编辑对应共享文件，禁止与 `branch`/`grep`/`diff` 并发修改同一文件（G-10） |

### 发布分组与并发窗口

默认独立发布切片（G-07）；以下四组必须同批交付交互/状态边界的切片按 G-08 登记为家族卡：

| ID | 成员 | 唯一发布点 | 窗口规则 | 失败回滚顺序 | 理由 |
|---|---|---|---|---|---|
| REL-CSR-01 | `CSR-01`, `CSR-23`, `CSR-25` | `CSR-25` | `CSR-01`/`CSR-23` 本地签名提交且不推送、不 bump；窗口期禁止插入其它切片；`CSR-25` 统一跑 `T-2` 全量与 `patch + 1` 发布 | `CSR-23` → `CSR-01`（本地 `revert`）或 `CSR-25` 后 `immutable-release` 前滚 | `restore -p` 全交互键与 `--source` 扩展必须同版发布 |
| REL-CSR-03 | `CSR-08`, `CSR-28`, `CSR-31` | `CSR-31` | `CSR-08`/`CSR-28` 本地签名提交且不推送、不 bump；窗口期禁止插入其它切片；`CSR-31` 统一跑 `T-2` 全量与 `patch + 1` 发布 | `CSR-28` → `CSR-08`（本地 `revert`）或 `CSR-31` 后 `immutable-release` 前滚 | `checkout` ahead/behind 输出与 `--detach`/`--quiet`/`--json`/缺引用抑制必须同版发布 |
| REL-CSR-04 | `CSR-02`, `CSR-27`, `CSR-03`, `CSR-32` | `CSR-32` | `CSR-02`/`CSR-27`/`CSR-03` 本地签名提交且不推送、不 bump；窗口期禁止插入其它切片；`CSR-32` 统一跑 `T-2` 全量与 `patch + 1` 发布 | `CSR-03` → `CSR-27` → `CSR-02`（本地 `revert`）或 `CSR-32` 后 `immutable-release` 前滚 | `checkout -p`、辅助选项、`--overlay` 默认与 `-p --overlay` 互斥必须同版发布 |
| REL-CSR-05 | `CSR-09`, `CSR-33`, `CSR-24`, `CSR-34` | `CSR-34` | `CSR-09`/`CSR-33`/`CSR-24` 本地签名提交且不推送、不 bump；窗口期禁止插入其它切片；`CSR-34` 统一跑 `T-2` 全量与 `patch + 1` 发布。`CSR-35` 已合回 `CSR-09`，不再占发布槽 | `CSR-24` → `CSR-33` → `CSR-09`（本地 `revert`）或 `CSR-34` 后 `immutable-release` 前滚 | `--pathspec-from-file=<file>` 与其全部互斥、`-` 标准输入与 `--pathspec-file-nul` 必须同版发布 |

- **并发声明:** 全串行（按 G-10 禁止 I–I 并发）。
- **发布者:** Plan Owner / 仓库维护者 `Eli`（作为本计划单一串行发布负责人，由执行本计划的 Agent 在其授权下串行执行各独立发布卡与家族发布点的 `patch + 1`、`gh release create`、D 组远端门证据收集及家族 `../libra-backend` `cf` 分支统一推送；`family child` 卡仅本地验收、跑 `fmt`+`clippy` 并本地签名提交，不在发布点之前推送 `cf`）。
- **发布窗口顺序:** `CSR-25`（REL-CSR-01）→ `CSR-32`（REL-CSR-04）→ `CSR-04` → … → `CSR-31`（REL-CSR-03）→ `CSR-34`（REL-CSR-05：`CSR-09`+`CSR-33`+`CSR-24`）→ `CSR-10` → `CSR-11` → `CSR-12` → … → `CSR-22`，同一时刻仅允许 1 个发布单元处于已 bump 未完成推送/发布状态（ER-12）。

### Phase 0: Patch 模式、路径恢复与参数校验（CSR-01..CSR-09, CSR-23..CSR-25, CSR-27..CSR-28, CSR-31..CSR-34；CSR-35 已废弃）

**目标:** 交付 `restore -p`、`checkout -p`、`--overlay` 语义与诊断、`--ours`/`--theirs`、`--no-overlay <tag>`、`.` 消歧与进度选项、`--detach`+`-b` 互斥、追踪状态打印与 `--pathspec-from-file` / `--pathspec-file-nul`。

**进入条件:**

- 本计划通过 Codex review（`VERDICT: PASS`），且 `DEP-CSR-01` 基线 parity 预检通过。

**退出条件:**

- `CSR-01`..`CSR-09`、`CSR-23`..`CSR-25`、`CSR-27`..`CSR-28`、`CSR-31`..`CSR-34` 全部达到 `Lifecycle=done`、`Acceptance=complete`。废弃编号 `CSR-35` 不占完成门。

### Phase 1: 追踪继承、符号链接安全与合并切换（CSR-10 .. CSR-16, CSR-29）

**目标:** 交付 `--track=direct|inherit`、`-f` 符号链接安全删除、`-m` 合并切换、`status.rs` D/F 修复、`--force` 替换前导目录链接、`--orphan <start-point>` 与 unborn `-b`。

**进入条件:**

- Phase 0 退出条件满足。

**退出条件:**

- `CSR-10`..`CSR-16`、`CSR-29` 全部完成，且命中 `T-1` 的卡（`CSR-10`、`CSR-11`、`CSR-12`、`CSR-13`、`CSR-29`、`CSR-14`）已通过对应全量 nextest。

### Phase 2: DWIM、`-B` 重置、D/F 冲突与 `switch -C` 收口（CSR-17 .. CSR-22, CSR-30）

**目标:** 交付多远端 DWIM 歧义、`-B` 保留暂存改动、唯一远端引用 DWIM、非空工作区目录替换、`merge` D/F stage 1/2 记录与 `switch -C` unborn HEAD，并完成收口全量门。

**进入条件:**

- Phase 1 退出条件满足。

**退出条件:**

- 33 张活动卡（`CSR-01`..`CSR-25`、`CSR-27`..`CSR-34`，不含已废弃的 `CSR-35`）全部 `done/complete`，收口全量门全绿。

## 任务卡

### 字段全局默认与例外

- **Release boundary 默认:** `independent`
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（每卡独立发布 `v<version>` tag 与 Cloudflare artifact 不可撤回；失败一律 `patch + 1` 前滚修复，严禁删 tag。降级指引：受影响用户可通过 `install.sh` 或包管理器回退至上一已知良好 `v<prev>` release；兼容窗口：保持 CLI/SQLite/索引格式向后兼容，支持直接降级至上一 patch 版本而无需数据迁移）
- **Migration and rollback 默认:** `N/A：无 schema 迁移`
- **Security and privacy 默认:** `继承 GC-07、GC-11、GC-CSR-02`
- **Performance budget 默认:** `继承 GC-10`

**D 组远端后置门默认（只适用于 `independent` 与 `family release point`，不适用于 `family child`，ER-04）：**
- 触发与证据：PR 事件和 `main` push 事件分开取证，不要求两边是同一个 SHA。PR 记录 PR head SHA、该 PR 的 head ref，以及目标分支 `main`。`push` 记录 `gh pr merge --squash` 产生的 merge commit SHA，ref 为 `main`。用该 PR 的合并记录证明两个 SHA 属于同一次交付。每一项再记录 workflow 文件、job 名、触发事件、`conclusion=success` 和 run URL。`cancel-superseded-main.yml` 取消的 run 不算 PASS，须等待该事件自己的后续全绿 run。家族子卡不跑本组。
- `base.yml` 的 `pull_request`：在该 PR head SHA 上，job `format`、`clippy`、`redundancy`、`test` 全部 `success`。目标分支是 `main`。
- `base.yml` 的 `push`：在 squash merge 后的 `main` SHA 上，同样四个 job 全部 `success`。ref 是 `main`。
- `codeql.yml` 的 `pull_request`：在该 PR head SHA 上，job `analyze` 的矩阵项 `language=actions` 与 `language=rust` 都 `success`。目标分支是 `main`。
- `codeql.yml` 的 `push`：在同一个 squash merge 的 `main` SHA 上，同样两个矩阵项都 `success`。ref 是 `main`。
- `release.yml`，事件 `push`，ref 为 `gh release create` 推上的 `v*` tag：job `build-and-upload`（linux/amd64、linux/arm64、darwin/arm64、windows/amd64 四条矩阵腿）、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` 全部 `success`。这五项都没有合法跳过条件；任一腿或任一 job 失败即 D 组失败。
- CDN 可获取性与完整性：对同一 tag 版本，使用 `curl -fsSL --retry 3 -o <file> -w '%{http_code}' <url>` 完整 GET `libra-linux-amd64`、`libra-linux-arm64`、`libra-darwin-arm64`、`libra-windows-amd64.exe`、Windows 签名清单实际使用的无后缀 `libra-windows-amd64`，以及 `install.sh`/`install.ps1` 和 `https://download.libra.tools/libra/releases/stable/manifest-v1.json`；HEAD 请求不计通过。先按安装器同一 pinned key、版本和有效期验证 manifest 签名，再逐一核对四个平台条目的 URL、`size` 与 `sha256`：Linux amd64/arm64 与 Darwin arm64 的实际下载内容必须与各自条目一致；Windows 条目只承诺无后缀 `libra-windows-amd64` URL，先核对该对象的实际字节数与 SHA-256，再单独下载 `libra-windows-amd64.exe` 并将其实际字节数和 SHA-256 与同一条目比较，不要求 manifest 含 `.exe` URL 或第五条产物记录。安装脚本须非空且 `install.sh` 通过 `sh -n`。临时目录完成后删除。记录 manifest URL/签名结果、每个产物 URL、HTTP 状态、字节数与 SHA-256；任一失败则本卡保持 `remote-pending`，按 ER-09/ER-10 处理，不得标 `complete`。不得依赖发布流程未上传的 `.size` sidecar。

**规则 waiver（`EX-*`）：**

| 例外 ID | 任务（或 `ALL/<作用域>`） | 豁免项 | 理由与补偿措施 | Approver | Review round | 证据 | 有效期 |
|---|---|---|---|---|---|---|---|
| EX-CSR-03 | CSR-09 | G-03 条目上限（门族型验收） | `--pathspec-from-file=<file>` 的读取与全部互斥校验是同一恢复轴。按门拆成 `CSR-35` 违反 G-02，故合回本卡。15 个门各自是可复制的具名 `nextest` 命令，失败即整卡不达标；清单写在卡内「判据规范（非计数正文）」。G-02 不可豁免。 | Eli（仓库维护者 / Plan Owner）于 2026-11-17 书面批准 | R23 | 本计划修订历史 2026-11-17 08:28:29 UTC 行：Eli 批准 EX-CSR-03，作为本计划内的具名审批记录 | 本计划内 |

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| CSR-01 | implementation | `restore -p` D15 解禁与 `y`/`n`/`q`/互斥 | revert本地提交 | yes | yes | 8/8 | 2/8 | 3/3 | M | DEP-CSR-01,DEP-CSR-02,DEP-CSR-03,DEP-CSR-05,DEP-CSR-07 | 串行起点 | family child | N/A | N/A |
| CSR-23 | implementation | `restore -p` `a`/`d`/`s`/`e` 与 `--source` | revert本地提交 | yes | yes | 8/8 | 2/8 | 3/3 | M | CSR-01,DEP-CSR-02,DEP-CSR-03,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-01 | family child | CSR-01 | N/A |
| CSR-25 | release | `REL-CSR-01` 家族发布点 | patch+1前滚修复 | yes | yes | 5/12 | 5/12 | 0/0 | S | CSR-23,DEP-CSR-07 | 序列化于 CSR-23 | family release point | N/A | N/A |
| CSR-02 | implementation | `checkout -p/--patch` 核心与互斥 | revert本地提交 | yes | yes | 6/8 | 2/8 | 3/3 | M | CSR-25,DEP-CSR-02,DEP-CSR-03,DEP-CSR-07 | 序列化于 CSR-25 | family child | N/A | N/A |
| CSR-27 | implementation | `checkout -p` `--auto-advance`/`-U`/inter-hunk | revert本地提交 | yes | yes | 7/8 | 3/8 | 2/2 | M | CSR-02,DEP-CSR-02,DEP-CSR-03,DEP-CSR-07 | 序列化于 CSR-02 | family child | CSR-02 | N/A |
| CSR-03 | implementation | `checkout` overlay 默认与诊断 | revert本地提交 | yes | yes | 6/8 | 2/8 | 1/1 | M | CSR-27,DEP-CSR-02,DEP-CSR-07 | 序列化于 CSR-27 | family child | N/A | N/A |
| CSR-32 | release | `REL-CSR-04` 家族发布点 | patch+1前滚修复 | yes | yes | 6/12 | 5/12 | 0/0 | S | CSR-03,DEP-CSR-07 | 序列化于 CSR-03 | family release point | N/A | N/A |
| CSR-04 | implementation | `checkout --theirs/--ours` 选边 | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 1/1 | M | CSR-32,DEP-CSR-02,DEP-CSR-07 | 序列化于 CSR-32 | independent | N/A | N/A |
| CSR-05 | implementation | `--no-overlay <tag>` 源树解析 | patch+1前滚修复 | yes | yes | 5/8 | 2/8 | 2/2 | M | CSR-04,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-04 | independent | N/A | N/A |
| CSR-06 | implementation | `.` 路径消歧与进度选项 | patch+1前滚修复 | yes | yes | 4/8 | 2/8 | 2/2 | M | CSR-05,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-05 | independent | N/A | N/A |
| CSR-07 | implementation | `--detach` 与 `-b`/`-B` 互斥 | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 1/1 | M | CSR-06,DEP-CSR-02,DEP-CSR-07 | 序列化于 CSR-06 | independent | N/A | N/A |
| CSR-08 | implementation | `checkout` 打印 ahead/behind 状态 | revert本地提交 | yes | yes | 8/8 | 2/8 | 1/1 | M | CSR-07,DEP-CSR-02,DEP-CSR-07 | 序列化于 CSR-07 | family child | N/A | N/A |
| CSR-28 | implementation | `checkout` 状态 `--detach`/`--quiet`/`--json`/缺引用 | revert本地提交 | yes | yes | 4/8 | 2/8 | 1/1 | M | CSR-08,DEP-CSR-02,DEP-CSR-07 | 序列化于 CSR-08 | family child | CSR-08 | N/A |
| CSR-31 | release | `REL-CSR-03` 家族发布点 | patch+1前滚修复 | yes | yes | 5/12 | 3/12 | 0/0 | S | CSR-28,DEP-CSR-07 | 序列化于 CSR-28 | family release point | N/A | N/A |
| CSR-09 | implementation | `--pathspec-from-file=<file>` 读取与全部互斥 | revert本地提交 | yes | yes | 15/8@EX-CSR-03 | 15/8@EX-CSR-03 | 2/2 | M | CSR-31,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-31 | family child | N/A | EX-CSR-03 |
| CSR-33 | implementation | `--pathspec-from-file=-` 标准输入读取 | revert本地提交 | yes | yes | 3/8 | 2/8 | 2/2 | M | CSR-09,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-09 | family child | CSR-09 | N/A |
| CSR-24 | implementation | `--pathspec-file-nul` NUL 切分与报错 | revert本地提交 | yes | yes | 6/8 | 2/8 | 2/2 | M | CSR-33,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-33 | family child | CSR-09 | N/A |
| CSR-34 | release | `REL-CSR-05` 家族发布点 | patch+1前滚修复 | yes | yes | 5/12 | 4/12 | 0/0 | S | CSR-24,DEP-CSR-07 | 序列化于 CSR-24 | family release point | N/A | N/A |
| CSR-10 | implementation | `--track=inherit/direct` 与配置 | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 2/2 | M | CSR-34,DEP-CSR-02,DEP-CSR-06,DEP-CSR-07 | 序列化于 CSR-34 | independent | N/A | N/A |
| CSR-11 | implementation | `restore.rs:2206` 逐组件符号链接安全与常规/索引回归 | patch+1前滚修复 | yes | yes | 8/8 | 8/8 | 3/3 | M | CSR-10,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-10 | independent | N/A | N/A |
| CSR-12 | implementation | `checkout -m` 合并切换与冲突恢复 | patch+1前滚修复 | yes | yes | 8/8 | 3/8 | 3/3 | M | CSR-11,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-11 | independent | N/A | N/A |
| CSR-13 | implementation | `status` 宽容 D/F 与普通 `checkout HEAD` | patch+1前滚修复 | yes | yes | 6/8 | 2/8 | 3/3 | M | CSR-12,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-12 | independent | N/A | N/A |
| CSR-29 | implementation | `checkout --force HEAD` / `switch -f` 恢复 D/F 与 `F.t` | patch+1前滚修复 | yes | yes | 6/8 | 2/8 | 3/3 | M | CSR-13,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-13 | independent | CSR-13 | N/A |
| CSR-14 | implementation | `--force` 替换前导目录符号链接 | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 3/3 | M | CSR-29,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-29 | independent | N/A | N/A |
| CSR-15 | implementation | `checkout --orphan` 接收 `<start-point>` | patch+1前滚修复 | yes | yes | 7/8 | 2/8 | 1/1 | M | CSR-14,DEP-CSR-02,DEP-CSR-07 | 序列化于 CSR-14 | independent | N/A | N/A |
| CSR-16 | implementation | unborn orphan 上 `checkout -b` 成功 | patch+1前滚修复 | yes | yes | 4/8 | 2/8 | 1/1 | M | CSR-15,DEP-CSR-02,DEP-CSR-07 | 序列化于 CSR-15 | independent | N/A | N/A |
| CSR-17 | implementation | 多远端 DWIM 歧义不回退 pathspec | patch+1前滚修复 | yes | yes | 5/8 | 3/8 | 2/2 | M | CSR-16,DEP-CSR-02,DEP-CSR-07 | 序列化于 CSR-16 | independent | N/A | N/A |
| CSR-18 | implementation | `checkout -B` 保留已暂存可合并改动与共享预检回归 | patch+1前滚修复 | yes | yes | 5/8 | 3/8 | 2/2 | M | CSR-17,DEP-CSR-02,DEP-CSR-07 | 序列化于 CSR-17 | independent | N/A | N/A |
| CSR-19 | implementation | 单一远程引用 DWIM 解析 | patch+1前滚修复 | yes | yes | 6/8 | 2/8 | 2/2 | M | CSR-18,DEP-CSR-02,DEP-CSR-07 | 序列化于 CSR-18 | independent | N/A | N/A |
| CSR-20 | implementation | 普通模式非空目录替换为跟踪文件 | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 3/3 | M | CSR-19,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-19 | independent | N/A | N/A |
| CSR-30 | implementation | `--force` 替换非空目录与清理空目录 | patch+1前滚修复 | yes | yes | 4/8 | 2/8 | 3/3 | M | CSR-20,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07 | 序列化于 CSR-20 | independent | CSR-20 | N/A |
| CSR-21 | implementation | `merge` D/F 记录 `destdir~HEAD` stage 1/2 | patch+1前滚修复 | yes | yes | 7/8 | 2/8 | 2/2 | M | CSR-30,DEP-CSR-02,DEP-CSR-04,DEP-CSR-07 | 序列化于 CSR-30 | independent | N/A | N/A |
| CSR-22 | implementation | `switch -C` 在 unborn HEAD 正常工作 | patch+1前滚修复 | yes | yes | 4/8 | 2/8 | 1/1 | M | CSR-21,DEP-CSR-02,DEP-CSR-07 | 序列化于 CSR-21 | independent | N/A | N/A |

---

### Task CSR-01: 实现 `libra restore -p/--patch` D15 解禁、核心 `y`/`n`/`q` 交互与互斥（#1037 部分 1/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #1037 核心交互与 D15 解禁轴（与 `CSR-23` 同属 `REL-CSR-01` 联合发布）：解除 `src/cli.rs:3028-3048` D15 表对 `restore -p/--patch` 的拒绝（同步 `tests/compat/compat_declined_interactive_flags.rs` 与 `docs/development/commands/_compatibility.md`），复用 `src/internal/patch_mode/` 在 `src/command/restore.rs` 落地工作区 discard 写入器（保持 `patch_mode` 对 `add -p`/`reset -p` 的无工作区写入不变量），实现 `y`/`n`/`q`、`Discard` 提示、`--staged` 与 `--pathspec-from-file` 互斥（TC-2624）。
**Out of scope:** `a`/`d`/`s`/`e` 控制键与 `--source=HEAD`/`HEAD^`/`HEAD^...`/`-- dir`（拆出 `CSR-23`，同属 `REL-CSR-01` 联合发布）；`checkout -p`（由 `CSR-02` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `src/internal/patch_mode/` 仅写暂存区（`mod.rs:1-6`）；`restore -p` 在 `src/cli.rs:3028-3048` D15 表被拒；`restore.rs:480-520` 缺少工作区 discard 写入器与 TC-2624 互斥检查 | `src/internal/patch_mode/mod.rs:1-6`、`src/cli.rs:3028-3048`、`src/command/restore.rs:480-520`、`tests/compat/compat_declined_interactive_flags.rs` |
**Acceptance criteria:**
- [ ] 移除 `src/cli.rs:3028-3048` D15 表对 `restore -p` 的拒绝，使 `tests/compat/compat_declined_interactive_flags.rs` D15 守卫通过。
- [ ] `libra restore -p <path>` 交互输入 `y` 将选中 hunk 写入工作区（丢弃对应改动）。
- [ ] `libra restore -p <path>` 交互输入 `n` 保留工作区对应 hunk 改动。
- [ ] `libra restore -p <path>` 交互输入 `q` 立即中止剩余 hunk 并保持未确认改动不动。
- [ ] 工作区恢复交互提示包含 `Discard` 文案。
- [ ] `libra restore -p --staged <path>` 按 hunk 更新暂存区。
- [ ] `libra restore -p --pathspec-from-file=<f>` 退出 128（TC-2624）。
- [ ] `libra restore -p --pathspec-from-file=<f>` 输出与 Git 一致的互斥错误（TC-2624）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。网站 `../libra-backend` `cf` 推送由 `CSR-25` 统一承载。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::restore_test::restore_patch_mode`（new）
- [ ] `source .env.test && cargo nextest run --test compat_declined_interactive_flags`
**Full-suite trigger:** N/A（family child，由 `CSR-25` 统一执行 `T-2`）
**Dependencies:** DEP-CSR-01, DEP-CSR-02, DEP-CSR-03, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/cli.rs`, `src/internal/patch_mode/`, `src/command/restore.rs`, `tests/command/restore_test.rs`, `tests/compat/compat_declined_interactive_flags.rs`, `tests/compat/README.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `docs/development/commands/_compatibility.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/restore.md`、`docs/commands/zh-CN/restore.md`、`docs/development/commands/restore.md`、`docs/development/commands/_compatibility.md`（D15 行）、`tests/compat/README.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** revert
**Estimated scope:** M（3 个落点，3 个生产文件，公开交互接口与 D15 解禁）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-01 (1/2)
**C/D coverage from:** CSR-25
**Granularity:** `type=implementation; axis=restore-pD15解禁与y/n/q/互斥; recovery=revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=DEP-CSR-01,DEP-CSR-02,DEP-CSR-03,DEP-CSR-05,DEP-CSR-07; writeset=串行起点; release=family child; split-from=N/A; exception=N/A`

---

### Task CSR-23: 补齐 `restore -p` `a`/`d`/`s`/`e` 控制键与 `--source` 恢复（#1037 部分 2/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #1037 扩展控制键与 `--source` 恢复轴（自 `CSR-01` 拆出，同属 `REL-CSR-01` 联合发布）：确保 `libra restore -p` 独立支持 `a`/`d`/`s`/`e` hunk 控制键，并支持 `--source=HEAD`/`HEAD^`/`HEAD^...`（merge-base）与 `-- dir` 目录限定。
**Out of scope:** D15 解禁、`y`/`n`/`q` 基础交互、`--staged` 与 `--pathspec-from-file` 互斥（已在 `CSR-01` 实现）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `restore.rs:480-520` 缺少 `--source=HEAD`/`HEAD^`/`HEAD^...` 与 `a`/`d`/`s`/`e` 集成断言 | `src/command/restore.rs:480-520`、`src/internal/patch_mode/mod.rs` |
**Acceptance criteria:**
- [ ] `libra restore -p <path>` 支持 `a`（全部接受当前文件剩余 hunk）并写入工作区。
- [ ] `libra restore -p <path>` 支持 `d`（跳过当前文件剩余 hunk）并保留原内容。
- [ ] `libra restore -p <path>` 支持 `s`（将大 hunk 拆分为更小 hunk 逐块确认）。
- [ ] `libra restore -p <path>` 支持 `e`（通过编辑器修改 hunk 补丁后应用）。
- [ ] `libra restore -p --source=HEAD <path>` 按 `HEAD` 交互恢复（TC-2607）。
- [ ] `libra restore -p --source=HEAD^ <path>` 按 `HEAD^` 交互恢复（TC-2608）。
- [ ] `libra restore -p --source=HEAD^...`（merge-base 语法）正确解析基线提交并交互恢复。
- [ ] `libra restore -p --source=HEAD^ -- dir` 仅交互提示 `dir/` 下变更文件（TC-2614）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。网站 `../libra-backend` `cf` 推送由 `CSR-25` 统一承载。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::restore_test::restore_patch_controls`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::restore_test::restore_patch_source`（new）
**Full-suite trigger:** N/A（family child，由 `CSR-25` 统一执行 `T-2`）
**Dependencies:** CSR-01, DEP-CSR-02, DEP-CSR-03, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/cli.rs`, `src/internal/patch_mode/`, `src/command/restore.rs`, `tests/command/restore_test.rs`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/restore.md`、`docs/commands/zh-CN/restore.md`、`docs/development/commands/restore.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** revert
**Estimated scope:** M（3 个落点，3 个生产文件，公开交互控制键与 `--source` 恢复）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-01 (2/2)
**C/D coverage from:** CSR-25
**Granularity:** `type=implementation; axis=restore-p控制键a/d/s/e与--source; recovery=revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-01,DEP-CSR-02,DEP-CSR-03,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-01; release=family child; split-from=CSR-01; exception=N/A`

---

### Task CSR-25: `REL-CSR-01` (`restore -p` 完整交互与互斥) 家族发布点

**Task type:** release
**Lifecycle / Acceptance:** pending / 空
**Description:** 聚合 `CSR-01` 与 `CSR-23`（`REL-CSR-01`），执行 `T-2` 双环境完整 nextest、版本面三处同步、`patch + 1`、`gh release create` 与 D 组门验收，作为 `restore -p` 的唯一发布承载点（G-08）。
**Out of scope:** 不引入新产品代码改动。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CSR-01` 与 `CSR-23` 同属 `restore -p` 家族，须合并于同一版本发布 | `REL-CSR-01` |
**Acceptance criteria:**
- [ ] `CSR-01` 本地提交已合入同一工作树。
- [ ] `CSR-23` 本地提交已合入同一工作树。
- [ ] `REL-CSR-01` release note 已生成并登记。
- [ ] `CSR-01`/`CSR-23` 关联文档与兼容交付证据已汇总登记。
- [ ] 家族子卡网站文档已在已验证的 `../libra-backend` `cf` 分支推送，并记录推送前后 SHA。
（ER-04/ER-08 强制门按 G-03 不计入本卡特有 AC/VER 分子。）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::restore_test::restore_patch_mode`
- [ ] `source .env.test && cargo nextest run --test command_test command::restore_test::restore_patch_controls`
- [ ] `source .env.test && cargo nextest run --test command_test command::restore_test::restore_patch_source`
- [ ] `source .env.test && cargo nextest run --test compat_declined_interactive_flags`
- [ ] `libra log -n 2 --oneline`（手工核对 `CSR-01` 与 `CSR-23` 提交均在发布树中）
**Full-suite trigger:** T-2: `family release point`（执行双环境完整 nextest）
**Dependencies:** CSR-23, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `tests/INDEX.md`（仅需同步时）, `docs/development/plan/plan-20261117-checkout-switch-restore.md`
**Release write set:** Inherited
**Files likely touched:** 版本面三处、`Cargo.lock`
**Docs and compatibility impact:** 汇总 `CSR-01`/`CSR-23` 文档交付证据并记录 release note。
**Rollback mode:** immutable-release
**Estimated scope:** S（0 个生产文件，仅家族发布点聚合）
**Version increment:** patch
**Release boundary:** family release point of REL-CSR-01
**C/D coverage from:** self
**Granularity:** `type=release; axis=REL-CSR-01家族发布点; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/12; VER=5/12; landing=0; prod-files=0; scope=S; deps=CSR-23,DEP-CSR-07; writeset=序列化于CSR-23; release=family release point; split-from=N/A; exception=N/A`

---

### Task CSR-02: 实现 `libra checkout -p/--patch` 核心交互检出与互斥校验（#702 部分 1/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #702 中 `-p`/`--patch` 核心交互轴：在 `CheckoutArgs` 注册 `-p`/`--patch`，同步解除 `src/cli.rs:3040` D15 对 `checkout -p` 的旧拒绝条目，并更新 `tests/compat/compat_declined_interactive_flags.rs` 与 `tests/compat/README.md:97`。
**Out of scope:** `--auto-advance`/`--no-auto-advance`/`--unified`/`--inter-hunk-context`（拆出 `CSR-27`）；`--overlay` 语义与 `-p --overlay` 互斥诊断（由 `CSR-03` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `checkout -p` 在 `src/cli.rs:3040` 被 D15 拒绝，`tests/compat/compat_declined_interactive_flags.rs` 与 `tests/compat/README.md:97` 仍登记该限制 | `src/cli.rs:3040`、`tests/compat/compat_declined_interactive_flags.rs`、`tests/compat/README.md:97`、`src/command/checkout.rs:53-120` |
**Acceptance criteria:**
- [ ] `libra checkout -p [-- <path>]` 按 hunk 交互式恢复工作区。
- [ ] `libra checkout -p <rev> [-- <path>]` 按 hunk 交互式同步更新索引与工作区。
- [ ] `checkout -p -b <branch>` 返回 exit 128。
- [ ] `checkout -p -B <branch>` 返回 exit 128。
- [ ] `checkout -p --orphan <branch>` 返回 exit 128。
- [ ] `tests/compat/compat_declined_interactive_flags.rs` D15 守卫移除 `checkout -p` 后通过。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。网站 `../libra-backend` `cf` 推送由 `CSR-32` 统一承载。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::checkout_patch_mode`（new）
- [ ] `source .env.test && cargo nextest run --test compat_declined_interactive_flags`
**Full-suite trigger:** N/A（family child，由 `CSR-32` 统一执行 `T-2`）
**Dependencies:** CSR-25, DEP-CSR-02, DEP-CSR-03, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/cli.rs`, `src/internal/patch_mode/`, `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `tests/compat/compat_declined_interactive_flags.rs`, `tests/compat/README.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/development/commands/_compatibility.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`docs/development/commands/_compatibility.md`、`tests/compat/README.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** revert
**Estimated scope:** M（3 个落点，3 个生产文件，新增公开交互接口）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-04 (1/3)
**C/D coverage from:** CSR-32
**Granularity:** `type=implementation; axis=checkout-p/--patch核心与互斥; recovery=revert本地提交; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-25,DEP-CSR-02,DEP-CSR-03,DEP-CSR-07; writeset=序列化于CSR-25; release=family child; split-from=N/A; exception=N/A`

---

### Task CSR-27: 补齐 `checkout -p` `--auto-advance`/`-U`/`--inter-hunk-context`（#702 部分 2/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #702 中 `-p` 辅助选项轴（自 `CSR-02` 拆出）：在 `CheckoutArgs` 注册 `--auto-advance`/`--no-auto-advance`、`--unified=<n>`（`-U`）与 `--inter-hunk-context=<n>`，并解除 `src/cli.rs:3051,3062` D15 对 `--no-auto-advance`/`--auto-advance` 的拒绝条目及更新 `src/cli.rs:5320`（`declined_interactive_table_maps_checkout_no_auto_advance`）。
**Out of scope:** `checkout -p` 核心交互流程（已在 `CSR-02` 交付）；`--overlay`（由 `CSR-03` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `--no-auto-advance`/`--auto-advance` 在 `src/cli.rs:3051,3062` 被 D15 拒绝且由 `src/cli.rs:5320` 单测守卫，`CheckoutArgs` 缺少 `-U`/`--inter-hunk-context` | `src/cli.rs:3051,3062,5320`、`src/command/checkout.rs:53-120` |
**Acceptance criteria:**
- [ ] `checkout -p --auto-advance` 选项被接受并传递至 `patch_mode`。
- [ ] `checkout -p --no-auto-advance` 选项被接受并传递至 `patch_mode`。
- [ ] `checkout -p --unified=<n>` 选项被接受。
- [ ] `checkout -p --inter-hunk-context=<n>` 选项被接受。
- [ ] `src/cli.rs:5320`（`declined_interactive_table_maps_checkout_no_auto_advance`）及关联 D15 单测同步更新并通过。
- [ ] 固定 30 行文件的第 5、15 行修改：`-U2` 时输出精确 2 行上下文且第 5 行 hunk 与第 15 行 hunk 之间有 5 行未修改间隔；`--inter-hunk-context=5` 时合并为一个 hunk，`=4` 时分裂为两个 hunk；`-U0` 时无上下文。仅应用第 5 行 hunk 后，文件只撤销第 5 行修改而保留第 15 行修改，禁止以“输出不同”代替这些精确边界断言。
- [ ] `--auto-advance` 与 `--no-auto-advance` 分别实际启用和抑制交互自动推进，并有交互行为断言。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。网站 `../libra-backend` `cf` 推送由 `CSR-32` 统一承载。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::checkout_patch_options`（new；使用上述固定夹具逐项断言上下文、合并/分裂边界和最终文件内容，并断言 auto-advance 与 no-auto-advance 对交互推进的实际影响）
- [ ] `source .env.test && cargo nextest run --test compat_declined_interactive_flags`
- [ ] `source .env.test && cargo nextest run --lib cli::tests`
**Full-suite trigger:** N/A（family child，由 `CSR-32` 统一执行 `T-2`）
**Dependencies:** CSR-02, DEP-CSR-02, DEP-CSR-03, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/cli.rs`, `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `tests/compat/compat_declined_interactive_flags.rs`, `tests/compat/README.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/development/commands/_compatibility.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`docs/development/commands/_compatibility.md`、`tests/compat/README.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** revert
**Estimated scope:** M（2 个落点，2 个生产文件，新增公开 `-p` 辅助选项）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-04 (2/3)
**C/D coverage from:** CSR-32
**Granularity:** `type=implementation; axis=checkout-p--auto-advance/-U/inter-hunk; recovery=revert本地提交; complete=yes; self-contained=yes; AC=7/8; VER=3/8; landing=2; prod-files=2; scope=M; deps=CSR-02,DEP-CSR-02,DEP-CSR-03,DEP-CSR-07; writeset=序列化于CSR-02; release=family child; split-from=CSR-02; exception=N/A`

---

### Task CSR-03: 对齐 `checkout` 默认 `overlay` 语义与 `-p --overlay` 冲突诊断（#702 部分 3/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #702 中 `--overlay` 部分（ADR-CSR-02）：将 `checkout <tree> -- <path>` 默认行为对齐 Git 的 `--overlay`（不删除源树中缺失的已跟踪文件），注册 `--overlay` 与 `--no-overlay` 最后生效覆盖，并对齐 `options '-p' and '--overlay' cannot be used together` 诊断。
**Out of scope:** `-p`/`--patch` 交互流程（已在 `CSR-02` 交付）；`--no-overlay <tag>`（由 `CSR-05` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `checkout.rs:102-107,720-722` 委托 `restore` 采用了 `--no-overlay` 默认，且缺少 `--overlay` 选项 | `src/command/checkout.rs:102-107,720-722` |
**Acceptance criteria:**
- [ ] `libra checkout <rev> -- <path>` 默认采用 overlay 模式（保留源树中不存在的已跟踪文件）。
- [ ] `libra checkout --no-overlay <rev> -- <path>` 删除源树中缺失的已跟踪文件。
- [ ] `--no-overlay --overlay` 以后者为准保留文件。
- [ ] `libra checkout -p --overlay` 退出 `128`。
- [ ] `libra checkout -p --overlay` 输出 `options '-p' and '--overlay' cannot be used together`。
- [ ] `libra checkout --overlay -b foo` 返回互斥错误。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。网站 `../libra-backend` `cf` 推送由 `CSR-32` 统一承载。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::checkout_overlay_and_diagnostics`（new）
- [ ] `source .env.test && cargo nextest run --lib command::checkout`
**Full-suite trigger:** N/A（family child，由 `CSR-32` 统一执行 `T-2`）
**Dependencies:** CSR-27, DEP-CSR-02, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** revert
**Estimated scope:** M（1 个落点，1 个生产文件，公开默认恢复模式变更）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-04 (3/3)
**C/D coverage from:** CSR-32
**Granularity:** `type=implementation; axis=checkout_overlay默认与诊断; recovery=revert本地提交; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CSR-27,DEP-CSR-02,DEP-CSR-07; writeset=序列化于CSR-27; release=family child; split-from=N/A; exception=N/A`

---

### Task CSR-04: 支持 `checkout --theirs` / `--ours` 合并端选项（#703）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #703：在 `CheckoutArgs` 注册 `--ours`（`-2`）与 `--theirs`（`-3`），复用 `restore` 冲突阶段恢复逻辑，包含 D/F 冲突下 `--theirs --no-overlay -- file1` 删除 `file1` 场景。
**Out of scope:** `--merge`（`-m`）分支切换合并（由 `CSR-12` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CheckoutArgs` 缺少 `--ours` / `--theirs`，而 `RestoreArgs` 已支持 | `src/command/checkout.rs:53-120`、`src/command/restore.rs:287` |
**Acceptance criteria:**
- [ ] `libra checkout --ours -- <path>` 将 stage 2 写入工作区。
- [ ] `libra checkout --theirs -- <path>` 使该路径与 stage 3 一致（their 端删除时路径消失）。
- [ ] `--ours` 与 `<rev>` 联用时 fail-closed 报错。
- [ ] `--theirs` 与 `<rev>` 联用时 fail-closed 报错。
- [ ] `--ours` 与 `-b` 联用时 fail-closed 报错。
- [ ] `--theirs` 与 `-b` 联用时 fail-closed 报错。
- [ ] `--ours` 与 `-B` 联用时 fail-closed 报错。
- [ ] `--theirs` 与 `-B` 联用时 fail-closed 报错。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::checkout_ours_theirs`（new）
- [ ] `source .env.test && cargo nextest run --lib command::checkout`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** CSR-32, DEP-CSR-02, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，新增公开 CLI 选项）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=checkout--theirs/--ours选边; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CSR-32,DEP-CSR-02,DEP-CSR-07; writeset=序列化于CSR-32; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-05: 修复 `checkout --no-overlay <tag>` 解析 tag 为源树（#704）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #704：修复 `checkout --no-overlay <tag> -- <path>`（含轻量 tag 与附注 tag）时剥离 tag 到 tree/commit 源树解析失败的问题。
**Out of scope:** `--overlay` 冲突诊断（已在 `CSR-03` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `restore_checkout_paths` / `restore.rs` 源树解析在 `--no-overlay <tag>` 场景未能剥离 tag | `src/command/checkout.rs:706-757` |
**Acceptance criteria:**
- [ ] `libra checkout --no-overlay <lightweight-tag> -- *file3` 正确匹配子目录路径并恢复对应文件内容（逐字节比对一致）。
- [ ] `libra checkout --no-overlay <annotated-tag> -- *file3` 正确匹配子目录路径并恢复对应文件内容（逐字节比对一致）。
- [ ] 上述 `--no-overlay <tag>` 恢复删除源树中缺失的已跟踪路径。
- [ ] 无 `--` 写法 `libra checkout --no-overlay <tag> <path>` 同样成功。
- [ ] 无效 tag 名时 fail-closed 报错并退出非零。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::no_overlay_tag_source`（new）
- [ ] `source .env.test && cargo nextest run --lib command::restore`
**Full-suite trigger:** T-1: 修改共享恢复引擎 `src/command/restore.rs`
**Dependencies:** CSR-04, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件，修复公开源树解析行为）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--no-overlay<tag>源树解析; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CSR-04,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-04; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-06: 修复 `checkout --no-progress .` 路径消歧与进度标志（#705）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #705：修复 `index_matches_path` 对 `.`（及子目录）不匹配导致 `libra checkout --no-progress .` 误进入切分支流程并被未跟踪文件拒绝的问题，并补充 `--progress` / `--quiet --progress` 与 `--no-progress` 覆盖行为。
**Out of scope:** 待恢复路径自身被未跟踪文件占用的真实碰撞（仍应拒绝）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `index_matches_path` 仅按字面条目匹配，`.` 未匹配索引前缀而落入分支切换 | `src/command/checkout.rs:695-704` |
**Acceptance criteria:**
- [ ] 工作区存在未跟踪文件且已跟踪文件被删除时，`libra checkout --no-progress .`（无 `--`）成功恢复已删除跟踪文件。
- [ ] 未跟踪文件保持完整不被删除或覆盖。
- [ ] `--no-progress` 选项被接受并正常恢复路径（Libra 无 `filter.<driver>.process` 延迟过滤器，对应上游进度测试归入 `DEFER-CSR-02`）。
- [ ] `--quiet --progress` 选项组合被接受并正常恢复路径。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::restore_deleted_with_untracked_present`（new）
- [ ] `source .env.test && cargo nextest run --lib command::restore`
**Full-suite trigger:** T-1: 修改共享恢复引擎 `src/command/restore.rs`
**Dependencies:** CSR-05, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件，修复公开路径消歧行为）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=.路径消歧与进度选项; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CSR-05,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-05; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-07: 拒绝 `checkout --detach` 与 `-b`/`-B`/`--orphan` 联用（#706）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #706：在 `CheckoutArgs` 添加互斥校验，当 `--detach` 与 `-b`/`-B`/`--orphan` 同时出现时返回用法错误。
**Out of scope:** 单独 `checkout --detach [<commit>]` 行为。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CheckoutArgs.detach` 未设置 `conflicts_with` 约束 | `src/command/checkout.rs:57-77` |
**Acceptance criteria:**
- [ ] `libra checkout --detach -b foo` 退出 `128`。
- [ ] `libra checkout --detach -b foo` 输出 `'--detach' cannot be used with '-b/-B/--orphan'`。
- [ ] `libra checkout --detach -b foo` 不创建分支 `foo`。
- [ ] `libra checkout --detach -B foo` 退出 `128`。
- [ ] `libra checkout --detach -B foo` 不创建分支 `foo`。
- [ ] `libra checkout --detach --orphan foo` 退出 `128`。
- [ ] `libra checkout --detach --orphan foo` 不改变 `HEAD`。
- [ ] `libra checkout --detach [<branch>]` 保持既有分离头指针行为。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::detach_incompatible_with_branch_create`（new）
- [ ] `source .env.test && cargo nextest run --lib command::checkout`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** CSR-06, DEP-CSR-02, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，公开参数互斥校验）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--detach与-b/-B/--orphan互斥; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CSR-06,DEP-CSR-02,DEP-CSR-07; writeset=序列化于CSR-06; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-08: `libra checkout` 输出分支追踪 ahead/behind 状态（#707 部分 1/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #707 中追踪状态文本与流向轴：在 `checkout` 切换到带上游追踪配置的本地分支（含 `branch.<name>.remote = .` 本地追踪）时，输出 `Your branch is up to date with ...` / `ahead of ...` / `behind ...` / `have diverged` 与 `pull` 提示行至 stdout，并将 `Switched to branch ...` 输出至 stderr。
**Out of scope:** `checkout --track=inherit`（由 `CSR-10` 承接）；`--detach`/`--quiet`/`--json`/追踪引用缺失降级（拆出 `CSR-28`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `render_checkout_output` 仅打印 `Switched to branch ...`，未输出 ahead/behind 状态 | `src/command/checkout.rs:1229-1286` |
**Acceptance criteria:**
- [ ] 分支追踪远程上游且持平时输出 `Your branch is up to date with ...`。
- [ ] 分支追踪本地上游（`remote=.`）且持平时输出 `Your branch is up to date with ...`。
- [ ] 分支领先上游时输出 `Your branch is ahead of ...`。
- [ ] 分支落后上游时输出 `Your branch is behind ...`。
- [ ] 分支与上游分叉时输出 `Your branch and ... have diverged`。
- [ ] behind 时输出 `pull` 提示行。
- [ ] diverged 时输出 `pull` 提示行。
- [ ] `Switched to branch ...` 输出至 stderr（对齐 `t2020` `test_cmp expect stdout`）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。网站 `../libra-backend` `cf` 推送由 `CSR-31` 统一承载。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::checkout_prints_tracking_status`（new）
- [ ] `source .env.test && cargo nextest run --lib command::checkout`
**Full-suite trigger:** N/A（family child，由 `CSR-31` 统一执行 `T-2`）
**Dependencies:** CSR-07, DEP-CSR-02, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** revert
**Estimated scope:** M（1 个落点，1 个生产文件，公开输出行为变化）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-03 (1/2)
**C/D coverage from:** CSR-31
**Granularity:** `type=implementation; axis=checkout输出ahead/behind状态; recovery=revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CSR-07,DEP-CSR-02,DEP-CSR-07; writeset=序列化于CSR-07; release=family child; split-from=N/A; exception=N/A`

---

### Task CSR-28: `checkout` 追踪状态 `--detach`/`--quiet`/`--json` 与缺引用降级（#707 部分 2/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #707 中抑制与降级轴（自 `CSR-08` 拆出）：确保 `--detach` 与 `--quiet` 不输出追踪状态行、`--json` 保持合法 JSON，且追踪引用缺失时优雅降级不中断切换。
**Out of scope:** ahead/behind 文本格式化（已在 `CSR-08` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `render_checkout_output` 需区分 `--detach`/`--quiet`/`--json` 与缺失追踪 ref 场景 | `src/command/checkout.rs:1229-1286` |
**Acceptance criteria:**
- [ ] `libra checkout --detach <branch>` 不输出上游追踪状态行。
- [ ] `libra checkout --quiet <branch>` 不输出上游追踪状态行。
- [ ] `libra checkout --json <branch>` 输出合法 JSON 格式。
- [ ] 追踪引用缺失时优雅降级不报错中断切换。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。网站 `../libra-backend` `cf` 推送由 `CSR-31` 统一承载。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::checkout_tracking_status_suppression_and_json`（new）
- [ ] `source .env.test && cargo nextest run --lib command::checkout`
**Full-suite trigger:** N/A（family child，由 `CSR-31` 统一执行 `T-2`）
**Dependencies:** CSR-08, DEP-CSR-02, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** revert
**Estimated scope:** M（1 个落点，1 个生产文件，公开状态抑制与 JSON 契约）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-03 (2/2)
**C/D coverage from:** CSR-31
**Granularity:** `type=implementation; axis=checkout状态抑制与缺引用降级; recovery=revert本地提交; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CSR-08,DEP-CSR-02,DEP-CSR-07; writeset=序列化于CSR-08; release=family child; split-from=CSR-08; exception=N/A`

---

### Task CSR-09: 支持 `checkout --pathspec-from-file=<file>` 读取与全部互斥校验（#708 部分 1/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #708 中 `--pathspec-from-file=<file>` 读取、缺失诊断，以及与 `--detach`、位置 pathspec、`-p`/`--patch`、`-b`、`-B`、`--orphan` 的互斥（同一恢复轴，原 `CSR-35` 已合回本卡）。按 `EX-CSR-03` 门族型验收登记，避免再拆出纯校验卡违反 G-02。
**Out of scope:** `--pathspec-from-file=-`（G-09：自本卡拆出 `CSR-33`）；`--pathspec-file-nul`（由 `CSR-24` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CheckoutArgs` 无 `--pathspec-from-file` | `src/command/checkout.rs:53-120` |
**Acceptance criteria:**
- [ ] `--pathspec-from-file=<file>` 按行读取命名文件并恢复对应文件。
- [ ] `--pathspec-from-file=<file>` 缺失文件时退出 128。
- [ ] `--pathspec-from-file=<file>` 缺失文件时 stderr 含缺失诊断。
- [ ] `--pathspec-from-file=<file>` 与 `--detach` 联用时退出 128。
- [ ] `--pathspec-from-file=<file>` 与 `--detach` 联用时 stderr 含互斥错误。
- [ ] `--pathspec-from-file=<file>` 与位置 pathspec 联用时退出 128。
- [ ] `--pathspec-from-file=<file>` 与位置 pathspec 联用时 stderr 含互斥错误。
- [ ] `--pathspec-from-file=<file>` 与 `-p`/`--patch` 联用时退出 128。
- [ ] `--pathspec-from-file=<file>` 与 `-p`/`--patch` 联用时 stderr 含互斥错误。
- [ ] `--pathspec-from-file=<file>` 与 `-b` 联用时退出 128。
- [ ] `--pathspec-from-file=<file>` 与 `-b` 联用时 stderr 含互斥错误。
- [ ] `--pathspec-from-file=<file>` 与 `-B` 联用时退出 128。
- [ ] `--pathspec-from-file=<file>` 与 `-B` 联用时 stderr 含互斥错误。
- [ ] `--pathspec-from-file=<file>` 与 `--orphan` 联用时退出 128。
- [ ] `--pathspec-from-file=<file>` 与 `--orphan` 联用时 stderr 含互斥错误。
- [ ] 同卡完成本地文档逐文件验收；网站 `../libra-backend` `cf` 推送由 `CSR-34` 统一承载（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_reads_lines`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_missing_exit_128`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_missing_stderr`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_detach_exit_128`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_detach_stderr`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_positional_exit_128`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_positional_stderr`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_patch_exit_128`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_patch_stderr`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_b_exit_128`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_b_stderr`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_force_branch_exit_128`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_force_branch_stderr`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_orphan_exit_128`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_orphan_stderr`（new）

**判据规范（非计数正文，15 门；分子 `15/8@EX-CSR-03`）:**
- 门 1（按行读取命名文件并恢复对应文件）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_reads_lines`
- 门 2（缺失文件时退出 128）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_missing_exit_128`
- 门 3（缺失文件时 stderr 含缺失诊断）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_missing_stderr`
- 门 4（与 `--detach` 联用时退出 128）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_detach_exit_128`
- 门 5（与 `--detach` 联用时 stderr 含互斥错误）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_detach_stderr`
- 门 6（与位置 pathspec 联用时退出 128）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_positional_exit_128`
- 门 7（与位置 pathspec 联用时 stderr 含互斥错误）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_positional_stderr`
- 门 8（与 `-p`/`--patch` 联用时退出 128）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_patch_exit_128`
- 门 9（与 `-p`/`--patch` 联用时 stderr 含互斥错误）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_patch_stderr`
- 门 10（与 `-b` 联用时退出 128）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_b_exit_128`
- 门 11（与 `-b` 联用时 stderr 含互斥错误）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_b_stderr`
- 门 12（与 `-B` 联用时退出 128）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_force_branch_exit_128`
- 门 13（与 `-B` 联用时 stderr 含互斥错误）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_force_branch_stderr`
- 门 14（与 `--orphan` 联用时退出 128）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_orphan_exit_128`
- 门 15（与 `--orphan` 联用时 stderr 含互斥错误）：`source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_file_orphan_stderr`
**Full-suite trigger:** N/A（family child，由 `CSR-34` 统一执行 `T-2`；本卡自跑 `fmt` + `clippy`）
**Dependencies:** CSR-31, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`（子卡完成本地更新与逐文件验收；网站 `cf` 推送由发布点承载）。
**Rollback mode:** revert
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-05 (1/3)
**C/D coverage from:** CSR-34
**Granularity:** `type=implementation; axis=--pathspec-from-file=<file>读取与全部互斥; recovery=revert本地提交; complete=yes; self-contained=yes; AC=15/8@EX-CSR-03; VER=15/8@EX-CSR-03; landing=2; prod-files=2; scope=M; deps=CSR-31,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-31; release=family child; split-from=N/A; exception=EX-CSR-03`

---

### Task CSR-35: 已合并回 CSR-09（废弃，不得开工）

**Status:** superseded by `CSR-09`（R22，G-02）。本编号不再拥有写集、验收标准或发布边界，不计入活动卡，也不进入 `REL-CSR-05`。

---

### Task CSR-33: 支持 `checkout --pathspec-from-file=-` 标准输入读取（#708 部分 3/4）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #708 中 `--pathspec-from-file=-` 标准输入读取轴（G-09：自 `CSR-09` 拆出；`REL-CSR-05` 家族子卡 2/3，与 `CSR-09`/`CSR-24` 经 `CSR-34` 联合发布）。
**Out of scope:** `--pathspec-from-file=<file>`（已在 `CSR-09` 实现）；`--pathspec-file-nul`（由 `CSR-24` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `checkout`/`restore` 已存在标准输入读取器（`restore.rs:383` 处理 `path == "-"`），但 `checkout` 尚未接入 `--pathspec-from-file=-` 参数及委托 | `src/command/checkout.rs:53-120`、`src/command/restore.rs:383` |
**Acceptance criteria:**
- [ ] `libra checkout --pathspec-from-file=-` 复用 `restore.rs:383` 现有读取器按行读取路径并恢复对应文件，不重复实现第二套 stdin 解析器。
- [ ] `libra checkout --pathspec-from-file=-` 在标准输入为空时退出 `0`。
- [ ] 上述空标准输入不修改工作区。
- [ ] 同卡完成本卡「Docs and compatibility impact」所列全部文件的本地更新并逐文件验收，包括网站文件 `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`。网站 `cf` 远端推送仍只由 `CSR-34` 承载（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_stdin`（new）
- [ ] `source .env.test && cargo nextest run --lib command::restore`
**Full-suite trigger:** N/A（family child，由 `CSR-34` 统一执行 `T-2`；本卡自跑 `fmt` + `clippy`）
**Dependencies:** CSR-09, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`（子卡完成本地更新与逐文件验收；网站 `cf` 推送由发布点承载）。
**Rollback mode:** revert
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-05 (2/3)
**C/D coverage from:** CSR-34
**Granularity:** `type=implementation; axis=--pathspec-from-file=-标准输入; recovery=revert本地提交; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CSR-09,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-09; release=family child; split-from=CSR-09; exception=N/A`

---

### Task CSR-24: 支持 `checkout --pathspec-file-nul` NUL 切分与单独报错（#708 部分 4/4）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #708 中 `--pathspec-file-nul` NUL 切分与单独报错轴（`REL-CSR-05` 家族子卡 3/3）：搭配 `--pathspec-from-file=<file>` 或 `-` 时按 NUL 切分路径，单独使用 `--pathspec-file-nul` 时退出 `128` 报错。
**Out of scope:** `--pathspec-from-file` 读取与互斥（已在 `CSR-09` 实现）；标准输入（已在 `CSR-33` 实现）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CheckoutArgs` 缺少 `--pathspec-file-nul` 与单独使用报错 | `src/command/checkout.rs:53-120`、`src/command/restore.rs` |
**Acceptance criteria:**
- [ ] `libra checkout --pathspec-from-file=<file> --pathspec-file-nul` 按 NUL 切分读取路径。
- [ ] 上述命令正确恢复被 NUL 切分命中的对应文件。
- [ ] `libra checkout --pathspec-from-file=- --pathspec-file-nul` 按 NUL 切分标准输入。
- [ ] 上述命令恢复被 NUL 切分命中的文件。
- [ ] `--pathspec-file-nul` 未搭配 `--pathspec-from-file` 时退出 `128`。
- [ ] `--pathspec-file-nul` 未搭配 `--pathspec-from-file` 时 stderr 输出 Git 一致错误。
- [ ] 同卡完成本卡「Docs and compatibility impact」所列全部文件的本地更新并逐文件验收，包括网站文件 `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`。网站 `cf` 远端推送仍只由 `CSR-34` 承载（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_file_nul`（new）
- [ ] `source .env.test && cargo nextest run --lib command::restore`
**Full-suite trigger:** N/A（family child，由 `CSR-34` 统一执行 `T-2`；本卡自跑 `fmt` + `clippy`）
**Dependencies:** CSR-33, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** N/A
**Files likely touched:** 同上
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`（子卡完成本地更新与逐文件验收；网站 `cf` 推送由发布点承载）。
**Rollback mode:** revert
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-05 (3/3)
**C/D coverage from:** CSR-34
**Granularity:** `type=implementation; axis=--pathspec-file-nul; recovery=revert本地提交; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CSR-33,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-33; release=family child; split-from=CSR-09; exception=N/A`

---

### Task CSR-34: `REL-CSR-05` (`--pathspec-from-file` 与 `--pathspec-file-nul`) 家族发布点

**Task type:** release
**Lifecycle / Acceptance:** pending / 空
**Description:** 聚合 `CSR-09`、`CSR-33` 与 `CSR-24`（`REL-CSR-05`；`CSR-35` 已合回 `CSR-09`），在三张子卡本地签名提交并完成本地 A/B/`fmt`/`clippy` 后，统一执行 `T-2` 双环境完整 nextest、版本面三处同步、`patch + 1`、`gh release create`、`../libra-backend` `cf` 推送与 D 组门验收（G-08）。
**Out of scope:** 不引入新产品代码改动。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CSR-09`/`33`/`24` 同属 `--pathspec-from-file` 完整选项边界，须同版发布 | `REL-CSR-05` |
**Acceptance criteria:**
- [ ] `CSR-09` 的本地提交已在发布工作树中。
- [ ] `CSR-33` 的本地提交已在发布工作树中。
- [ ] `CSR-24` 的本地提交已在发布工作树中。
- [ ] `REL-CSR-05` release note 已生成并登记。
- [ ] 三张子卡网站文档已在已验证的 `../libra-backend` `cf` 分支推送，并记录推送前后 SHA。
（ER-04/ER-08 强制门按 G-03 不计入本卡特有 AC/VER 分子。）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::pathspec_from_file_/)'`（覆盖 `CSR-09` 的 15 个具名门）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_from_stdin`
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::pathspec_file_nul`
- [ ] `libra log -n 10 --oneline`（核对 `CSR-09`/`33`/`24` 各自提交哈希均在发布树中）
**Full-suite trigger:** T-2: `family release point`（执行双环境完整 nextest）
**Dependencies:** CSR-24, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `tests/INDEX.md`（仅需同步时）, `docs/development/plan/plan-20261117-checkout-switch-restore.md`
**Release write set:** Inherited
**Files likely touched:** 版本面三处（`Cargo.toml`、`install.sh`、`install.ps1`）、`Cargo.lock`、`release artifact`
**Docs and compatibility impact:** 汇总 `CSR-09`/`33`/`24` 文档，推送 `../libra-backend` `cf` 并记录 release note。
**Rollback mode:** immutable-release
**Estimated scope:** S（0 个生产文件，仅家族发布点聚合）
**Version increment:** patch
**Release boundary:** family release point of REL-CSR-05
**C/D coverage from:** self
**Granularity:** `type=release; axis=REL-CSR-05联合发布; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/12; VER=4/12; landing=0; prod-files=0; scope=S; deps=CSR-24,DEP-CSR-07; writeset=序列化于CSR-24; release=family release point; split-from=N/A; exception=N/A`

---

### Task CSR-10: 支持 `checkout --track=inherit|direct` 与 `branch.autoSetupMerge=inherit`（#709）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #709：将 `CheckoutArgs.track` 升级为 `Option<BranchTrackMode>`（复用 `branch::BranchTrackMode`），支持 `--track=direct`、`--track=inherit` 与 `--no-track`，并在未显式指定时读取 `branch.autoSetupMerge`（含 `inherit`）配置。
**Out of scope:** `branch` 命令自身的 `--track`（已支持 `BranchTrackMode`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CheckoutArgs.track` 为 `bool` 且 `maybe_set_startpoint_upstream` 未读取 `branch.autoSetupMerge=inherit` | `src/command/checkout.rs:84-85,899-914` |
**Acceptance criteria:**
- [ ] `libra checkout -b new --track=inherit <base>` 复制 `<base>` 的上游配置。
- [ ] `libra checkout -b new --track=direct <base>` 直接将 `<base>` 设为上游。
- [ ] `branch.autoSetupMerge=true` 时未传 `--track` 自动设置追踪。
- [ ] `branch.autoSetupMerge=always` 时本地分支创建也自动设置追踪。
- [ ] `branch.autoSetupMerge=inherit` 时复制基线分支的上游配置。
- [ ] `branch.autoSetupMerge=false` 时不自动设置追踪。
- [ ] `--no-track` 覆盖配置禁用自动追踪。
- [ ] 无效 `--track=<bad>` 值返回用法错误。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::track_inherit_and_auto_setup_merge`（new）
- [ ] `source .env.test && cargo nextest run --lib command::checkout`
**Full-suite trigger:** T-1: 修改共享分支追踪逻辑 `src/command/branch.rs`
**Dependencies:** CSR-34, DEP-CSR-02, DEP-CSR-06, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/branch.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件，公开 CLI 选项与配置行为）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--track=inherit/direct与autoSetupMerge; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CSR-34,DEP-CSR-02,DEP-CSR-06,DEP-CSR-07; writeset=序列化于CSR-34; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-11: 修复 `restore.rs:2206` `-f` 逐组件符号链接不跟随与常规/索引回归（#712）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #712：修改 `checkout -f HEAD~`（`checkout.rs:1161` → `switch.rs:1412,1433`）与 `switch -f <target-branch>`（`switch.rs:1540,1607,1691`）共用的 `restore.rs:2206` 删除路径，对相对工作区根的**逐级路径组件（component-wise）**执行 `symlink_metadata` 检查（先检 `dir` 等前导父组件是否为符号链接，绝不对 `dir/f` 整串检查后穿过前导符号链接删除外部目标目录），并在同卡对两个入口验证常规已跟踪文件写入、无关未跟踪文件保留及索引一致性。
**Out of scope:** 非 `--force` 下的前导目录符号链接拦截（由 `CSR-14` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `restore.rs:2037-2054` helper 已用 `symlink_metadata`，但 `restore.rs:2206` 直接执行 `fs::remove_file(&path_abs)` 且整串路径检查无法拦截 `dir/f` 中的前导目录符号链接 `dir`；`checkout -f HEAD~` 走 `checkout.rs:1161`、`switch.rs:1412,1433`，`switch -f` 走 `switch.rs:1540,1607,1691` | `src/command/restore.rs:2206`、`src/command/checkout.rs:1161`、`src/command/switch.rs:1412,1433,1540,1607,1691` |
**Acceptance criteria:**
- [ ] `checkout -f HEAD~` 之后 `untracked/` 目录内容与执行前相同。
- [ ] `switch -f <target-branch>` 之后 `target_dir/untracked` 与执行前相同。
- [ ] `checkout -f HEAD~` 之后目标树的常规已跟踪文件已写入工作区。
- [ ] `checkout -f HEAD~` 之后无关未跟踪文件仍在工作区。
- [ ] `checkout -f HEAD~` 之后索引与目标树一致。
- [ ] `switch -f <target-branch>` 之后目标树的常规已跟踪文件已写入工作区。
- [ ] `switch -f <target-branch>` 之后无关未跟踪文件仍在工作区。
- [ ] `switch -f <target-branch>` 之后索引与目标树一致。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::force_checkout_preserves_symlink_target`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::switch_test::force_switch_preserves_symlink_target`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::force_checkout_writes_regular_files`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::force_checkout_preserves_untracked`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::force_checkout_syncs_index`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::switch_test::force_switch_writes_regular_files`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::switch_test::force_switch_preserves_untracked`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::switch_test::force_switch_syncs_index`（new）
- [ ] `source .env.test && cargo nextest run --lib command::restore`（ER-04 通用表面门，不计入 VER）
**Full-suite trigger:** T-1: 修改共享工作区恢复模块 `src/command/restore.rs` 与 `src/command/switch.rs`
**Dependencies:** CSR-10, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/checkout.md`, `docs/development/commands/switch.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/checkout.md`、`docs/development/commands/switch.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件，安全关键修复）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=restore.rs:2206逐组件符号链接安全与回归; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=3; prod-files=3; scope=M; deps=CSR-10,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-10; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-12: 支持 `checkout -m`（合并切换）并创建冲突阶段（#713）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #713：在 `CheckoutArgs` 注册 `-m`/`--merge`，当本地存在修改与目标分支冲突时执行三方合并，无冲突自动合并，有冲突写入冲突标记并在索引记录 stage 1/2/3；并验证随后 `checkout -f <branch>` 将冲突消解为 stage 0。
**Out of scope:** `D/F` 合并 `destdir~HEAD` stage 1/2（由 `CSR-21` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CheckoutArgs` 未注册 `-m`/`--merge` | `src/command/checkout.rs:53-120` |
**Acceptance criteria:**
- [ ] `libra checkout -m <branch>` 在本地修改可无冲突合并时切换至目标分支。
- [ ] 上述无冲突合并保留合并后的工作区内容。
- [ ] 存在行级冲突时 `libra checkout -m <branch>` 成功切换至目标分支。
- [ ] 存在行级冲突时工作区文件写入冲突标记。
- [ ] 存在行级冲突时索引记录 stage 1/2/3。
- [ ] 冲突后运行 `libra checkout -f <branch>` 清除冲突并在索引建立正确 stage 0 条目。
- [ ] `-f` 与 `-m` 同时传入时退出 128 并输出 `'-f' cannot be used with '-m'`。
- [ ] 合并阶段写入故障注入失败时，HEAD、原分支、索引与本地工作区修改均恢复到操作前状态，不得留下半写状态。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::merge_checkout_creates_conflict_stages`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::merge_checkout_failure_rolls_back`（new；故障注入验证 HEAD、原分支、索引与工作区修改均按恢复合同保留）
- [ ] `source .env.test && cargo nextest run --lib command::checkout`
**Full-suite trigger:** T-1: 修改共享切换/恢复模块 `src/command/switch.rs` 与 `src/command/restore.rs`
**Dependencies:** CSR-11, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件，新增公开 `-m` 合并切换）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=checkout-m合并切换与冲突恢复; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=3; prod-files=3; scope=M; deps=CSR-11,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-11; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-13: 修复跟踪目录被文件替换后 `status` 报 `LBR-IO-001` 与普通 `checkout HEAD` 保留（#714 部分 1/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #714 中 `status` 与普通 `checkout HEAD` 保留轴：修复 `src/command/status.rs:745` 在已跟踪目录被普通文件替换后报错 `LBR-IO-001`（`status output would be incomplete`）的问题，并确保普通 `libra checkout HEAD` 保持无破坏性（保留用户文件 `dir`、`F.t` 编辑与当前分支）。
**Out of scope:** `checkout --force HEAD` / `switch -f` 强制恢复（拆出 `CSR-29`）；非空工作区目录替换为跟踪文件（由 `CSR-20` 承接）；parallel-checkout worker 计数（归入 `DEFER-CSR-02`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `src/command/status.rs:745` 在跟踪目录被文件替换后返回 `LBR-IO-001`（`status output would be incomplete`） | `src/command/status.rs:745`、`src/command/checkout.rs`、`src/command/restore.rs` |
**Acceptance criteria:**
- [ ] 跟踪路径 `dir/file` 的 `dir` 被替换为普通文件后，`libra status` 不再报 `LBR-IO-001`。
- [ ] `libra status` 正确报告 `dir/file` 为 deleted。
- [ ] 普通 `libra checkout HEAD` 退出 0。
- [ ] 普通 `libra checkout HEAD` 保留工作区普通文件 `dir`。
- [ ] 普通 `libra checkout HEAD` 后 `HEAD` 仍留在原分支。
- [ ] 普通 `libra checkout HEAD` 保留独立跟踪文件 `F.t` 的本地编辑。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::tracked_dir_replaced_by_file_status_and_noop`（new）
- [ ] `source .env.test && cargo nextest run --lib command::status`
**Full-suite trigger:** T-1: 修改共享状态检查 `src/command/status.rs` 与 `src/command/restore.rs`
**Dependencies:** CSR-12, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/status.rs`, `src/command/checkout.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/status.en.md` 与 `COMPATIBILITY.md`。`docs/error-codes.md` 不修改：`LBR-IO-001` 仍用于其它不完整 status，本卡只停止「跟踪目录被文件替换」这条路径返回它。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件，修复公开工作区状态与无破坏性语义）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=status宽容D/F与普通checkoutHEAD; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-12,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-12; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-29: 支持 `checkout --force HEAD` / `switch -f` 恢复 D/F 与 `F.t`（#714 部分 2/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #714 中强制恢复轴（自 `CSR-13` 拆出）：确保 `libra checkout --force HEAD` 与 `libra switch -f` 删除冲突普通文件 `dir` 并恢复 `dir/file`，同时将 `F.t` 恢复为 `HEAD` 版本。
**Out of scope:** `status` D/F 宽容与普通 `checkout HEAD`（已在 `CSR-13` 交付）；parallel-checkout worker 计数（归入 `DEFER-CSR-02`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `checkout --force HEAD` 与 `switch -f` 在目录被文件替换场景受 `preflight_worktree_restore_to_commit`（`restore.rs:1941-1997`）无条件 `NonEmptyWorktreeDirectory` 拦截（于 `switch.rs:1433,1607` 调用） | `src/command/restore.rs:1941-1997`、`src/command/switch.rs:1433,1607`、`src/command/checkout.rs` |
**Acceptance criteria:**
- [ ] `libra checkout --force HEAD` 删除文件 `dir`。
- [ ] `libra checkout --force HEAD` 恢复 `dir/file`。
- [ ] `libra checkout --force HEAD` 将 `F.t` 恢复为 `HEAD` 版本。
- [ ] `libra switch -f <target-branch>`（含 `dir/file` 与 `F.t` 的目标分支）删除文件 `dir`。
- [ ] `libra switch -f <target-branch>` 恢复 `dir/file`。
- [ ] `libra switch -f <target-branch>` 将 `F.t` 恢复为目标分支版本。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::force_head_recovers_dir_and_ft`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::switch_test::tracked_dir_replaced_by_file`（new）
**Full-suite trigger:** T-1: 修改共享工作区恢复模块 `src/command/restore.rs` 与 `src/command/switch.rs`
**Dependencies:** CSR-13, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/checkout.md`, `docs/development/commands/switch.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件，修复公开 `--force HEAD` 恢复行为）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=checkout--forceHEAD与switch-f恢复; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-13,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-13; release=independent; split-from=CSR-13; exception=N/A`

---

### Task CSR-14: 允许 `checkout --force` 替换前导目录符号链接为真实目录（#715）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #715：当目标跟踪路径的前导目录在工作区是符号链接时，非 force 保持 fail-closed 拒绝，而 `checkout --force`（含 `--force HEAD`）与 `switch --force` 将前导符号链接 unlink（不删目标目录内容）并创建真实目录写入文件。
**Out of scope:** 非 `--force` 下的前导符号链接（保持 fail-closed 拒绝）；parallel-checkout worker 计数（归入 `DEFER-CSR-02`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `preflight_worktree_restore_to_commit`（`restore.rs:1941-1997`，于 `switch.rs:1433,1607` 无条件调用）忽略 `--force` 返回 `LBR-CONFLICT-002`/`NonEmptyWorktreeDirectory`，且 `restore.rs:1254-1260` 拒绝穿过符号链接父目录 | `src/command/restore.rs:1254-1260,1933-1997`、`src/command/switch.rs:1433,1607` |
**Acceptance criteria:**
- [ ] `dir` 为指向外部目录的符号链接且目标有 `dir/file` 时，`libra checkout --force <branch>` 移除符号链接 `dir`。
- [ ] 上述 `libra checkout --force <branch>` 写入真实目录 `dir/file`。
- [ ] `libra checkout --force HEAD` 移除符号链接 `dir`。
- [ ] `libra checkout --force HEAD` 写入真实目录 `dir/file`。
- [ ] 原符号链接指向的外部目录内容完好无损。
- [ ] 不带 `--force` 时仍拒绝穿过前导符号链接写入。
- [ ] `libra switch -f <branch>` 移除符号链接 `dir`。
- [ ] `libra switch -f <branch>` 写入真实目录 `dir/file`。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::force_replaces_leading_dir_symlink`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::switch_test::force_replaces_leading_dir_symlink`（new）
**Full-suite trigger:** T-1: 修改共享工作区恢复模块 `src/command/restore.rs` 与 `src/command/switch.rs`
**Dependencies:** CSR-29, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/checkout.md`, `docs/development/commands/switch.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件，公开 `--force` 语义与安全边界）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--force替换前导目录符号链接; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-29,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-29; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-15: 支持 `checkout --orphan <new-branch> [<start-point>]` 起点参数（#716）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #716：移除 `run_checkout` 中对 `checkout --orphan` 携带 `<start-point>` 的拒绝限制，将工作区与暂存区填充为 `<start-point>` 的内容，同时将 `HEAD` 设为指向 unborn `<new-branch>`。
**Out of scope:** `switch --orphan`（保持既有契约）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `run_checkout` 显式报错 `"checkout --orphan does not accept a start-point"` | `src/command/checkout.rs:461-470` |
**Acceptance criteria:**
- [ ] `libra checkout --orphan fresh <start-point>` 成功将 `HEAD` 切换为 unborn `fresh`。
- [ ] 上述命令后索引与 `<start-point>` 树一致。
- [ ] 上述命令后工作区与 `<start-point>` 树一致。
- [ ] 在该 unborn 分支上执行首次 `libra commit` 生成无父提交的 root commit。
- [ ] `<start-point>` 无效时 fail-closed 报错。
- [ ] `<start-point>` 无效时保持原 `HEAD` 与工作区不变。
- [ ] 不传 `<start-point>` 时保持现有保留索引/工作区行为。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::orphan_with_start_point`（new）
- [ ] `source .env.test && cargo nextest run --lib command::checkout`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** CSR-14, DEP-CSR-02, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，公开 CLI 参数扩展）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=checkout--orphan支持<start-point>; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CSR-14,DEP-CSR-02,DEP-CSR-07; writeset=序列化于CSR-14; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-16: 修复 unborn orphan 分支上 `checkout -b <new>` 成功切换（#718）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #718：修复 `resolve_checkout_create_startpoint`（`src/command/checkout.rs:960-969`）在当前处于 unborn orphan 分支且无 `<start-point>` 执行 `libra checkout -b <new>` 时错误拒绝的问题，使 `HEAD` 切换到新的 unborn 分支 `<new>`，且原 unborn 分支不存在。
**Out of scope:** `checkout --orphan <start-point>`（已在 `CSR-15` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `resolve_checkout_create_startpoint` 在 `Head::current_commit_result()` 为 `None` 时直接返回 `InvalidObjectName` 错误 | `src/command/checkout.rs:960-969` |
**Acceptance criteria:**
- [ ] 在 unborn orphan 分支上执行 `libra checkout -b anothername` 成功退出 0。
- [ ] 切换后 `HEAD` 指向 `refs/heads/anothername`（unborn）。
- [ ] 原 unborn 分支引用不存在。
- [ ] 已有提交时 `checkout -b <new>` 行为保持不变。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::checkout_b_from_unborn_orphan`（new）
- [ ] `source .env.test && cargo nextest run --lib command::checkout`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** CSR-15, DEP-CSR-02, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，修复公开 unborn `-b` 行为）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=unborn_orphan上checkout-b成功; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CSR-15,DEP-CSR-02,DEP-CSR-07; writeset=序列化于CSR-15; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-17: 修复多远端同名分支时 DWIM 歧义不再回退为 pathspec（#721）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #721：当裸名称 `<name>` 在多个远端均存在远程追踪分支时，`checkout <name>`（与 `switch <name>`）明确报告 `matched multiple (<N>) remote tracking branches` 歧义错误而不回退成 pathspec（文件保持未改动）。
**Out of scope:** 仅存在唯一远端匹配时的 DWIM 解析（由 `CSR-19` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `disambiguate_path_argument` 仅查本地分支与修订，多远端匹配失败后回落 `trailing_paths`；`switch.rs:179` 报错未包含计数 `(N)` | `src/command/checkout.rs:664-694`、`src/command/switch.rs:179,655` |
**Acceptance criteria:**
- [ ] 当 `origin/topic` 与 `upstream/topic` 同时存在且工作区存在同名文件 `topic` 时，`libra checkout topic` 失败退出非零。
- [ ] 上述失败时输出 `matched multiple (2) remote tracking branches`。
- [ ] 失败后工作区文件 `topic` 保持未改动。
- [ ] 显式 `libra checkout -- topic` 仍按 pathspec 处理。
- [ ] `switch topic` 输出一致的带计数歧义提示。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::dwim_ambiguous_remote_does_not_fallback_to_pathspec`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::switch_test::dwim_ambiguous_remote`（new）
- [ ] `source .env.test && cargo nextest run --lib command::switch`
**Full-suite trigger:** T-1: 修改共享 DWIM 解析逻辑 `src/command/switch.rs`
**Dependencies:** CSR-16, DEP-CSR-02, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/checkout.md`, `docs/development/commands/switch.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件，公开 DWIM 报错语义）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=多远端DWIM歧义不回退pathspec; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=2; scope=M; deps=CSR-16,DEP-CSR-02,DEP-CSR-07; writeset=序列化于CSR-16; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-18: 允许 `checkout -B` 在保留已暂存可合并改动下重置分支（#723）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #723：调整 `switch::ensure_switch_clean_or_force`（`src/command/switch.rs:852-867`）与 `checkout.rs:977`，允许 `libra checkout -B <branch> [<start-point>]` 在存在已暂存可合并改动时重置分支并保留该已暂存改动，同时保持 `checkout -b` / `switch -C` 既有用例正确。
**Out of scope:** 存在真实冲突的脏文件（仍应拒绝，除非 `-f` 或 `-m`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `switch::ensure_switch_clean_or_force` 拒绝脏树状态，连带阻塞 `checkout -B` 保留可合并暂存改动 | `src/command/switch.rs:852-867`、`src/command/checkout.rs:977-1000` |
**Acceptance criteria:**
- [ ] 当工作区存在可与 `<start-point>` 安全合并的已暂存改动时，`libra checkout -B <branch> [<start-point>]` 成功重置分支。
- [ ] 上述重置后已暂存改动完整保留。
- [ ] 当已暂存改动与目标提交冲突时，仍 fail-closed 拒绝重置。
- [ ] 干净工作区下 `-B` 行为保持不变。
- [ ] `checkout -b` 与 `switch -C` 的共享预检回归均通过；冲突拒绝前后分支、索引和工作区状态保持不变。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::force_branch_b_preserves_staged_mergeable_changes`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::switch_test::checkout_b_and_switch_c_conflict_preserves_state`（new；覆盖两个 CLI 入口及冲突拒绝前后的分支/索引/工作区状态）
- [ ] `source .env.test && cargo nextest run --lib command::switch`
**Full-suite trigger:** T-1: 修改共享切换预检 `src/command/switch.rs`
**Dependencies:** CSR-17, DEP-CSR-02, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件，修复公开 `-B` 行为）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=checkout-B保留已暂存可合并改动与共享预检回归; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=2; scope=M; deps=CSR-17,DEP-CSR-02,DEP-CSR-07; writeset=序列化于CSR-17; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-19: 支持 `checkout <branch>` 从唯一远程追踪引用 DWIM 解析（#724）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #724：移除 `checkout.rs:1002-1022` 中 `get_remote` 硬编码 `origin/<branch>` 与强制 `pull` 的逻辑，改为复用 `switch.rs:655` 的全远端 DWIM 解析（含 `checkout.defaultRemote`），使任意唯一远端均可直接基于本地已抓取远程追踪引用创建追踪分支。
**Out of scope:** 多远端歧义拒绝（已在 `CSR-17` 交付）；不新增未声明的 CLI 标志。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `checkout.rs` 中 `get_remote` 硬编码 `origin/{branch_name}` 并执行 `pull`，非 `origin` 远端无法 DWIM | `src/command/checkout.rs:1002-1022`、`src/command/switch.rs:655` |
**Acceptance criteria:**
- [ ] 当仅存在 `upstream/feat`（无 `origin/feat` 与本地 `feat`）时，`libra checkout feat` 基于 `upstream/feat` 创建本地 `feat`。
- [ ] 上述 `libra checkout feat` 为新建本地 `feat` 设置上游追踪至 `upstream/feat`。
- [ ] 配置 `checkout.defaultRemote` 且多远端存在同名分支时优先选用 `defaultRemote`。
- [ ] 离线（无网络）情况下基于本地已有远程追踪引用即可成功 DWIM，不再强行 `pull`。
- [ ] `--json` 输出 `pulled: false`（同步文档说明）。
- [ ] `--json` 输出 `tracking.remote` 为实际命中远端名（同步文档说明）。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::dwim_single_non_origin_remote`（new）
- [ ] `source .env.test && cargo nextest run --lib command::switch`
**Full-suite trigger:** T-1: 修改共享 DWIM 解析逻辑 `src/command/switch.rs`
**Dependencies:** CSR-18, DEP-CSR-02, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件，公开 DWIM 行为扩展）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=唯一远程引用DWIM解析; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CSR-18,DEP-CSR-02,DEP-CSR-07; writeset=序列化于CSR-18; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-20: 允许普通 `checkout`/`switch` 用跟踪文件替换非空工作区目录（#1008 部分 1/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #1008 中普通切换「非空工作区目录替换为跟踪文件」轴：当目标分支将原目录 `destdir` 变为普通跟踪文件 `destdir` 时，普通 `checkout` / `switch` 先删旧已跟踪文件与空目录、后写入新文件，遇未跟踪文件则 fail-closed 拒绝并保留目录内容。
**Out of scope:** `--force` 模式替换非空目录（拆出 `CSR-30`）；`merge` 中 `destdir~HEAD` 冲突阶段记录（由 `CSR-21` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `restore.rs:1601-1615` 与 `checkout.rs` 写回文件时遇到非空目录直接拒绝，未先清理将被切换移除的旧目录内容 | `src/command/restore.rs:1601-1615`、`src/command/checkout.rs` |
**Acceptance criteria:**
- [ ] 普通 `libra checkout <target>` 在 `destdir/` 仅含即将删除的已跟踪文件时删除旧目录。
- [ ] 上述普通 `libra checkout <target>` 写入文件 `destdir`。
- [ ] 普通 `libra switch <target>` 在 `destdir/` 仅含即将删除的已跟踪文件时删除旧目录。
- [ ] 上述普通 `libra switch <target>` 写入文件 `destdir`。
- [ ] 切换后无关本地修改保持完整。
- [ ] 切换后索引条目与目标树保持一致。
- [ ] 当 `destdir/` 内含未跟踪文件时普通模式 fail-closed 拒绝。
- [ ] 上述拒绝后 `destdir/` 原有未跟踪内容完整保留。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::replace_non_empty_tracked_dir_with_file`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::switch_test::replace_non_empty_tracked_dir_with_file`（new）
**Full-suite trigger:** T-1: 修改共享工作区恢复模块 `src/command/restore.rs` 与 `src/command/switch.rs`
**Dependencies:** CSR-19, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/checkout.md`, `docs/development/commands/switch.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件，公开 D/F 切换行为）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=普通模式非空工作区目录替换为跟踪文件; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-19,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-19; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-30: 支持 `--force` 替换非空工作区目录与清理空目录（#1008 部分 2/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #1008 中 `--force` 替换非空目录轴（自 `CSR-20` 拆出）：当 `destdir/` 包含阻塞普通切换的条目时，`checkout --force` 与 `switch -f` 按 Git 规则替换并清理空目录。
**Out of scope:** 普通模式非空目录替换（已在 `CSR-20` 交付）；`merge` `destdir~HEAD` stage 1/2（由 `CSR-21` 承接）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `preflight_worktree_restore_to_commit`（`restore.rs:1941-1997`，于 `switch.rs:1433,1607` 调用）忽略 `--force` 且 `restore.rs:1601-1615` 未在 `--force` 下先清理待移除目录内容 | `src/command/restore.rs:1601-1615,1933-1997`、`src/command/switch.rs:1433,1607` |
**Acceptance criteria:**
- [ ] `libra checkout --force <target>` 将非空 `destdir/` 替换为文件 `destdir`。
- [ ] `libra checkout --force <target>` 清理替换后残余空目录。
- [ ] `libra switch -f <target>` 将非空 `destdir/` 替换为文件 `destdir`。
- [ ] `libra switch -f <target>` 清理替换后残余空目录。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::force_replace_non_empty_dir_with_file`（new）
- [ ] `source .env.test && cargo nextest run --test command_test command::switch_test::force_replace_non_empty_dir_with_file`（new）
**Full-suite trigger:** T-1: 修改共享工作区恢复模块 `src/command/restore.rs` 与 `src/command/switch.rs`
**Dependencies:** CSR-20, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/checkout.md`, `docs/development/commands/switch.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件，公开 `--force` D/F 替换行为）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--force非空目录替换为跟踪文件; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-20,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07; writeset=序列化于CSR-20; release=independent; split-from=CSR-20; exception=N/A`

---

### Task CSR-21: 修复 `merge` D/F 冲突时 `destdir~HEAD` 记录 stage 1 与 stage 2（#1008 部分 3/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #1008 中「D/F merge 仅记录 stage 2 for `destdir~HEAD`」轴：在 `libra merge`（`src/command/merge.rs:4031` 与 `src/command/merge/rename_merge.rs`）处理 D/F 冲突时，将 `destdir~HEAD` 的索引记录为 **stage 1 与 stage 2**（对齐 Git `merge --strategy=recursive`），并抑制多余的 `modify/delete` 提示。
**Out of scope:** 非空工作区目录替换为跟踪文件（已由 `CSR-20` 交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `src/command/merge.rs:4031` 附近 D/F 冲突写入了不一致的阶段与额外 `modify/delete` 消息 | `src/command/merge.rs:4031`、`src/command/merge/rename_merge.rs` |
**Acceptance criteria:**
- [ ] `libra merge --strategy=recursive` 遇到 `#1008` D/F 冲突时，索引记录 `destdir~HEAD` 的 stage 1（`libra ls-files --stage` 验证）。
- [ ] `libra merge --strategy=recursive` 遇到 `#1008` D/F 冲突时，索引记录 `destdir~HEAD` 的 stage 2（`libra ls-files --stage` 验证）。
- [ ] 仅在 `#1008` 场景（rename/delete 与 `destdir~HEAD` D/F 冲突同时发生）下不额外输出多余的 `modify/delete` 提示。
- [ ] 常规 D/F 冲突仍按 `MP-17` 完整输出 `CONFLICT (modify/delete)`。
- [ ] 工作区保留文件 `destdir`。
- [ ] 其余冲突文件包含标准冲突标记。
- [ ] 普通无 D/F 冲突合并行为保持不变。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::merge_test::df_merge_records_stage_1_and_2`（new）
- [ ] `source .env.test && cargo nextest run --lib command::merge`
**Full-suite trigger:** T-1: 修改共享合并引擎 `src/command/merge.rs`
**Dependencies:** CSR-30, DEP-CSR-02, DEP-CSR-04, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/merge.rs`, `src/command/merge/rename_merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件，公开 D/F 合并索引状态）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=merge_DF记录destdir~HEAD_stage1与2; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CSR-30,DEP-CSR-02,DEP-CSR-04,DEP-CSR-07; writeset=序列化于CSR-30; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-22: 修复 `libra switch -C <branch>` 在 unborn HEAD 上执行成功（#1030）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #1030：当仓库处于 unborn HEAD（尚无任何提交）时，允许 `libra switch -C <branch>`（无 `<start-point>`）将 unborn `HEAD` 切换到新分支名 `<branch>`（保留索引/工作区），使 `t3404` 等初始仓库脚本正常建立分支。
**Out of scope:** 带显式 `<start-point>` 但 `<start-point>` 无效的错误（仍应 fail-closed）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `switch.rs:980` 中 `force_create`（`-C`）分支在无提交时解析 `HEAD` 失败 | `src/command/switch.rs:980` |
**Acceptance criteria:**
- [ ] 在刚 `libra init` 的空仓库（unborn `main`）上执行 `libra switch -C topic` 退出 `0`。
- [ ] 执行后 `HEAD` 指向 unborn `refs/heads/topic`。
- [ ] 随后首次 `libra commit` 创建的提交位于 `topic` 分支上。
- [ ] 在已有提交的仓库上 `switch -C <branch> [<start-point>]` 保持既有强制创建/重置行为。
- [ ] 同卡完成「Docs and compatibility impact」所列全部文档文件更新并逐文件验收通过（ER-06/ER-06a，不计入 G-03 卡特有 AC 分子）。
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::switch_test::switch_force_create_on_unborn_head`（new）
- [ ] `source .env.test && cargo nextest run --lib command::switch`
**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（执行阶段只跑本卡 focused 测试）
**Dependencies:** CSR-21, DEP-CSR-02, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `src/command/switch.rs`, `tests/command/switch_test.rs`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同上
**Docs and compatibility impact:** 更新 `docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md` 与 `COMPATIBILITY.md`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件，公开 `switch -C` unborn HEAD 行为）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=switch-C在unbornHEAD正常工作; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CSR-21,DEP-CSR-02,DEP-CSR-07; writeset=序列化于CSR-21; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-31: `REL-CSR-03` (`checkout` 追踪状态输出与抑制) 家族发布点

**Task type:** release
**Lifecycle / Acceptance:** pending / 空
**Description:** 聚合 `CSR-08` 与 `CSR-28`（`REL-CSR-03`），执行 `T-2` 双环境完整 nextest、版本面三处同步、`patch + 1`、`gh release create` 与 D 组门验收，作为 `CSR-08`/`CSR-28` 的唯一发布承载点（G-08）。
**Out of scope:** 不引入新产品代码改动。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CSR-08` 与 `CSR-28` 同属 `checkout` 追踪状态家族，须合并于同一版本发布 | `REL-CSR-03` |
**Acceptance criteria:**
- [ ] `CSR-08` 本地提交已合入同一工作树。
- [ ] `CSR-28` 本地提交已合入同一工作树。
- [ ] `REL-CSR-03` release note 已生成并登记。
- [ ] `CSR-08`/`CSR-28` 关联文档与兼容交付证据已汇总登记。
- [ ] 家族子卡网站文档已在已验证的 `../libra-backend` `cf` 分支推送，并记录推送前后 SHA。
（ER-04/ER-08 强制门按 G-03 不计入本卡特有 AC/VER 分子。）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::checkout_prints_tracking_status`
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::checkout_tracking_status_suppression_and_json`
- [ ] `libra log -n 2 --oneline`（手工核对 `CSR-08` 与 `CSR-28` 提交均在发布树中）
**Full-suite trigger:** T-2: `family release point`（执行双环境完整 nextest）
**Dependencies:** CSR-28, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `tests/INDEX.md`（仅需同步时）, `docs/development/plan/plan-20261117-checkout-switch-restore.md`
**Release write set:** Inherited
**Files likely touched:** 版本面三处、`Cargo.lock`
**Docs and compatibility impact:** 汇总 `CSR-08`/`CSR-28` 文档交付证据并记录 release note。
**Rollback mode:** immutable-release
**Estimated scope:** S（0 个生产文件，仅家族发布点聚合）
**Version increment:** patch
**Release boundary:** family release point of REL-CSR-03
**C/D coverage from:** self
**Granularity:** `type=release; axis=REL-CSR-03家族发布点; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/12; VER=3/12; landing=0; prod-files=0; scope=S; deps=CSR-28,DEP-CSR-07; writeset=序列化于CSR-28; release=family release point; split-from=N/A; exception=N/A`

---

### Task CSR-32: `REL-CSR-04` (`checkout -p` 与 `overlay` 默认/互斥) 家族发布点

**Task type:** release
**Lifecycle / Acceptance:** pending / 空
**Description:** 聚合 `CSR-02`、`CSR-27` 与 `CSR-03`（`REL-CSR-04`），执行 `T-2` 双环境完整 nextest、版本面三处同步、`patch + 1`、`gh release create` 与 D 组门验收，作为 `CSR-02`/`CSR-27`/`CSR-03` 的唯一发布承载点（G-08）。
**Out of scope:** 不引入新产品代码改动。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CSR-02`、`CSR-27` 与 `CSR-03` 同属 `checkout -p` 与 `overlay` 家族，须合并于同一版本发布 | `REL-CSR-04` |
**Acceptance criteria:**
- [ ] `CSR-02` 本地提交已合入同一工作树。
- [ ] `CSR-27` 本地提交已合入同一工作树。
- [ ] `CSR-03` 本地提交已合入同一工作树。
- [ ] `REL-CSR-04` release note 已生成并登记。
- [ ] `CSR-02`/`CSR-27`/`CSR-03` 关联文档与兼容交付证据已汇总登记。
- [ ] 家族子卡网站文档已在已验证的 `../libra-backend` `cf` 分支推送，并记录推送前后 SHA。
（ER-04/ER-08 强制门按 G-03 不计入本卡特有 AC/VER 分子。）
**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::checkout_patch_mode`
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::checkout_patch_options`
- [ ] `source .env.test && cargo nextest run --test command_test command::checkout_test::checkout_overlay_and_diagnostics`
- [ ] `source .env.test && cargo nextest run --test compat_declined_interactive_flags`
- [ ] `libra log -n 3 --oneline`（手工核对 `CSR-02`/`CSR-27`/`CSR-03` 提交均在发布树中）
**Full-suite trigger:** T-2: `family release point`（执行双环境完整 nextest）
**Dependencies:** CSR-03, DEP-CSR-07
**Deliverables:** N/A
**Implementation write set:** `tests/INDEX.md`（仅需同步时）, `docs/development/plan/plan-20261117-checkout-switch-restore.md`
**Release write set:** Inherited
**Files likely touched:** 版本面三处、`Cargo.lock`
**Docs and compatibility impact:** 汇总 `CSR-02`/`CSR-27`/`CSR-03` 文档交付证据并记录 release note。
**Rollback mode:** immutable-release
**Estimated scope:** S（0 个生产文件，仅家族发布点聚合）
**Version increment:** patch
**Release boundary:** family release point of REL-CSR-04
**C/D coverage from:** self
**Granularity:** `type=release; axis=REL-CSR-04家族发布点; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/12; VER=5/12; landing=0; prod-files=0; scope=S; deps=CSR-03,DEP-CSR-07; writeset=序列化于CSR-03; release=family release point; split-from=N/A; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `CheckoutArgs` 校验、pathspec-file 解析、DWIM 逻辑、hunk 模式 | `source .env.test && cargo nextest run --lib command::checkout` |
| 集成 | 全部 20 个 Issue 的 CLI 行为、patch 模式、`-m` 合并、`merge` D/F stage 1/2 与 unborn `-C` | `source .env.test && cargo nextest run --test command_test -E 'test(/^command::(checkout_test|switch_test|restore_test|merge_test)::/)'` |
| 兼容 | 版本面同步、D15 拒绝表守卫与 `git@6de20f6092` 对照语义 | `source .env.test && cargo nextest run --test compat_version_surface_sync && cargo nextest run --test compat_declined_interactive_flags` |
| 迁移 | N/A：无 SQLite schema 迁移 | N/A |
| 安全 | 符号链接不跟随删除、前导目录符号链接保护、普通 `checkout HEAD` 不删用户文件 | `source .env.test && cargo nextest run --test command_test command::checkout_test::` |
| 性能 | 继承 GC-10，索引与路径遍历无无界扫描 | N/A |
| live/gated | 计划收口双环境全量门 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| CSR-01 | Issue #1037 / `restore.rs:480-520` | `src/cli.rs`、`internal/patch_mode/`、`restore.rs` | `restore.md` EN+zh+web、`_compatibility.md`、`COMPATIBILITY.md` | `command::restore_test::restore_patch_mode` |
| CSR-23 | Issue #1037 / `restore.rs:480-520` | `src/cli.rs`、`internal/patch_mode/`、`restore.rs` | `restore.md` EN+zh+web、`COMPATIBILITY.md` | `command::restore_test::restore_patch_controls` / `restore_patch_source` |
| CSR-25 | `REL-CSR-01` 发布点 | `Cargo.toml`、`install.sh`、`install.ps1` | 发布汇总 | `restore_patch_*` + `compat_declined_interactive_flags` |
| CSR-02 | Issue #702 / `cli.rs:3028-3048` | `src/cli.rs`、`internal/patch_mode/`、`checkout.rs` | `checkout.md` EN+zh+web、`_compatibility.md`、`COMPATIBILITY.md` | `command::checkout_test::checkout_patch_mode` |
| CSR-03 | Issue #702 / `checkout.rs:102,720` | `src/command/checkout.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::checkout_overlay_and_diagnostics` |
| CSR-04 | Issue #703 / `checkout.rs:53-120` | `src/command/checkout.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::checkout_ours_theirs` |
| CSR-05 | Issue #704 / `checkout.rs:706-757` | `src/command/checkout.rs`、`src/command/restore.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::no_overlay_tag_source` |
| CSR-06 | Issue #705 / `checkout.rs:695-704` | `src/command/checkout.rs`、`src/command/restore.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::restore_deleted_with_untracked_present` |
| CSR-07 | Issue #706 / `checkout.rs:57-77` | `src/command/checkout.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::detach_incompatible_with_branch_create` |
| CSR-08 | Issue #707 / `checkout.rs:1229-1286` | `src/command/checkout.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::checkout_prints_tracking_status` |
| CSR-09 | Issue #708 / `checkout.rs:53-120` | `src/command/checkout.rs`、`src/command/restore.rs` | `checkout.md` EN+zh+dev+web、`COMPATIBILITY.md` | `command::checkout_test::pathspec_from_file_*` |
| CSR-33 | Issue #708 / `checkout.rs:53-120` | `src/command/checkout.rs`、`src/command/restore.rs` | `checkout.md` EN+zh+dev+web、`COMPATIBILITY.md` | `command::checkout_test::pathspec_from_stdin` |
| CSR-34 | `REL-CSR-05` 发布点 | `Cargo.toml`、`install.sh`、`install.ps1` | 发布汇总与 `cf` 推送 | `pathspec_from_file_*` + `pathspec_from_stdin` + `pathspec_file_nul` |
| CSR-35 | 已合回 `CSR-09` | N/A | N/A | N/A |
| CSR-10 | Issue #709 / `checkout.rs:84-85,899` | `src/command/checkout.rs`、`src/command/branch.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::track_inherit_and_auto_setup_merge` |
| CSR-11 | Issue #712 / `restore.rs:2206` | `src/command/checkout.rs`、`switch.rs`、`restore.rs` | `checkout.md` & `switch.md` EN+zh+web、`COMPATIBILITY.md` | `force_checkout_*` + `force_switch_*` |
| CSR-12 | Issue #713 / `checkout.rs:53-120` | `src/command/checkout.rs`、`switch.rs`、`restore.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::merge_checkout_creates_conflict_stages`、`command::checkout_test::merge_checkout_failure_rolls_back` |
| CSR-13 | Issue #714 / `status.rs:745` | `status.rs`、`checkout.rs`、`restore.rs` | `checkout.md` 与 `status.md` EN+zh+dev+网站、`COMPATIBILITY.md` | `command::checkout_test::tracked_dir_replaced_by_file_status_and_noop` |
| CSR-14 | Issue #715 / `restore.rs:1254` | `src/command/checkout.rs`、`switch.rs`、`restore.rs` | `checkout.md` & `switch.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::force_replaces_leading_dir_symlink` |
| CSR-15 | Issue #716 / `checkout.rs:461-470` | `src/command/checkout.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::orphan_with_start_point` |
| CSR-16 | Issue #718 / `checkout.rs:960-969` | `src/command/checkout.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::checkout_b_from_unborn_orphan` |
| CSR-17 | Issue #721 / `switch.rs:179,655` | `src/command/checkout.rs`、`src/command/switch.rs` | `checkout.md` & `switch.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::dwim_ambiguous_remote_does_not_fallback_to_pathspec` |
| CSR-18 | Issue #723 / `switch.rs:852-867` | `src/command/checkout.rs`、`src/command/switch.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::force_branch_b_preserves_staged_mergeable_changes`、`command::switch_test::checkout_b_and_switch_c_conflict_preserves_state` |
| CSR-19 | Issue #724 / `checkout.rs:1002-1022` | `src/command/checkout.rs`、`src/command/switch.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::dwim_single_non_origin_remote` |
| CSR-20 | Issue #1008 / `restore.rs:1601` | `src/command/checkout.rs`、`switch.rs`、`restore.rs` | `checkout.md` & `switch.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::replace_non_empty_tracked_dir_with_file` |
| CSR-21 | Issue #1008 / `merge.rs:4031` | `src/command/merge.rs`、`merge/rename_merge.rs` | `merge.md` EN+zh+web、`COMPATIBILITY.md` | `command::merge_test::df_merge_records_stage_1_and_2` |
| CSR-22 | Issue #1030 / `switch.rs:980` | `src/command/switch.rs` | `switch.md` EN+zh+web、`COMPATIBILITY.md` | `command::switch_test::switch_force_create_on_unborn_head` |
| CSR-24 | Issue #708 / `checkout.rs:53-120` | `src/command/checkout.rs`、`src/command/restore.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::pathspec_file_nul` |
| CSR-27 | Issue #702 / `cli.rs:3028-3070` | `src/cli.rs`、`src/command/checkout.rs` | `checkout.md` EN+zh+web、`_compatibility.md`、`COMPATIBILITY.md` | `command::checkout_test::checkout_patch_options` |
| CSR-28 | Issue #707 / `checkout.rs:1229-1286` | `src/command/checkout.rs` | `checkout.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::checkout_tracking_status_suppression_and_json` |
| CSR-29 | Issue #714 / `checkout.rs`、`restore.rs` | `checkout.rs`、`switch.rs`、`restore.rs` | `checkout.md` & `switch.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::force_head_recovers_dir_and_ft` |
| CSR-30 | Issue #1008 / `restore.rs:1601-1615` | `checkout.rs`、`switch.rs`、`restore.rs` | `checkout.md` & `switch.md` EN+zh+web、`COMPATIBILITY.md` | `command::checkout_test::force_replace_non_empty_dir_with_file` |
| CSR-31 | `REL-CSR-03` 发布点 | `Cargo.toml`、`install.sh`、`install.ps1` | 发布汇总 | `checkout_prints_tracking_status` + `checkout_tracking_status_suppression_and_json` |
| CSR-32 | `REL-CSR-04` 发布点 | `Cargo.toml`、`install.sh`、`install.ps1` | 发布汇总 | `checkout_patch_mode` + `checkout_patch_options` + `checkout_overlay_and_diagnostics` + `compat_declined_interactive_flags` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划评审 PASS，基线冻结（`62abb622...`，`0.30.207`） | Codex `PASS` | N/A |
| M1 | Phase 0 活动卡（`CSR-01`..`CSR-09`、`CSR-23`..`CSR-25`、`CSR-27`..`CSR-28`、`CSR-31`..`CSR-34`；不含已废弃的 `CSR-35`）完成且发布 | 4 个独立 patch release（`CSR-04`..`CSR-07`）+ 4 个家族发布点（`REL-CSR-01`/`03`/`04`/`05`）+ D 组全绿 | 按 `immutable-release` 前滚修复 |
| M2 | Phase 1 (`CSR-10`..`CSR-16`、`CSR-29`) 完成且 `T-1` 全量通过 | 8 个独立 patch release + nextest 全绿 | 按 `immutable-release` 前滚修复 |
| M3 | Phase 2 (`CSR-17`..`CSR-22`、`CSR-30`) 及收口全量门全绿 | 全部 33 张活动卡 `done/complete` | 按 `immutable-release` 前滚修复 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 符号链接安全检查触发异常 | 原符号链接及外部目标目录完好 | fail-closed 报错并保留现场 | 跟随符号链接删除或覆盖外部文件 |
| `-m` 合并切换写入冲突阶段失败 | 保持原分支与原索引 | 回滚索引与 HEAD 更新，提示用户恢复 | 留半写索引或丢失工作区修改 |
| `release.yml` 远端发布失败 | 已合并代码保留在 `main` | 按 ER-09/ER-10 新建 `FIX-*` 前滚修复 | 回退已推送 `main` 提交或删 tag |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| 符号链接与 D/F 替换误删未跟踪文件 | 高：用户数据丢失 | 统一使用 `symlink_metadata`；`CSR-13` 普通模式保留用户文件 | `CSR-11`, `CSR-13`, `CSR-29`, `CSR-14`, `CSR-20`, `CSR-30` |
| `restore.rs` / `switch.rs` / `merge.rs` / `status.rs` / `cli.rs` 改动影响面广 | 中：可能影响其他命令 | 独立发布卡按卡内 `Full-suite trigger` 的 `T-1` 跑全量；家族子卡（`CSR-01`/`02`/`03`/`08`/`09`/`23`/`24`/`27`/`28`/`33`；仅 `CSR-35` 废弃）不自跑全量，由发布点 `CSR-25`/`31`/`32`/`34` 的 `T-2` 覆盖 | `CSR-05`, `CSR-06`, `CSR-10`..`CSR-14`, `CSR-17`..`CSR-21`, `CSR-29`, `CSR-30`, `CSR-25`, `CSR-31`, `CSR-32`, `CSR-34` |
| `../libra-backend` 非标准 Git 目录导致写入前置受阻 | 低：阻塞网站文档自动同步 | 严格按 ER-06a 核对 VCS 与 `cf` 分支，遇阻立即 STOP 报告 | 全部 `CSR-*` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 路径/D/F 冲突预检 | $O(N)$（$N$ 为索引或变更条目数） | 单次切换 1 次遍历 | 无全仓递归无界扫描 | `command::checkout_test` |
| 交互式 hunk 切分 | $O(\text{diff lines})$ | 按修改文件线性处理 | 单文件受大文件保护 | `command::restore_test` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已随各卡同步更新 `checkout`、`switch`、`restore`、`merge` 行。
- [ ] `docs/commands/{checkout,switch,restore,merge}.md` 与 `zh-CN/` 文档已同步。
- [ ] `docs/development/commands/{checkout,switch,restore,merge,_compatibility}.md` 已同步。
- [ ] `docs/error-codes.md` 已核对/同步。
- [ ] `tests/INDEX.md` 与 `Cargo.toml` `[[test]]` 已核对（复用既有 `--test command_test`）。
- [ ] `plan-long.md` 日期计划索引已同步或确认 `N/A`。

## Codex review log

Result 只允许 `PASS` 或 `FAIL`。`FAIL` 必须列出 P0/P1 条目并在下一轮复审关闭；P2 可由具名责任人书面接受为 residual risk，但不改变本轮 `FAIL` 记录（ER-05）。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P0: `CSR-13` 普通 `checkout HEAD` 误删文件且漏 `status.rs:745`；P1: `CSR-01`/`02` 需复用 `internal/patch_mode/` 与改 `cli.rs` D15；`CSR-21` 需改 `merge.rs` stage 1/2；`CSR-16` unborn `-b` 应成功；`CSR-03` overlay 默认与文案；`CSR-06`/`09`/`12`/`18` 遗漏场景与写集 `T-1`；`immutable-release` 与原子 AC 计数 | R2 全部同步关闭 | Claude/Codex R1 只读评审 |
| R2 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: `base.yml` 含不存在的 `network-remotes`；跨计划写集缺 `DEP-CSR-03..06`；`CSR-02` 漏 `--auto-advance` D15；`CSR-11` 锚点应为 `:2206`；`CSR-12` 缺 `switch.rs`/`restore.rs` `T-1`；`CSR-01`/`09` G-03 原子计数需拆卡；`CSR-05` 缺 `*file3`；`CSR-08` 缺 stderr；外部子系统与脚本归入 `DEFER-CSR-02` | R3 全部同步关闭 | Claude/Codex R2 只读评审 |
| R3 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: `CSR-09`+`CSR-24` 与 `CSR-01`+`CSR-23` 拆分后独立发布导致参数校验滞后；P2: `CSR-01`/`09` `Granularity` AC 计数需同步 | R4 全部同步关闭 | Codex R3 只读评审（Claude R3 网络 ECONNRESET） |
| R4 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: `DEFER-CSR-02` 未登记；`DEP-CSR-03..06` 未接入卡片 `Dependencies`；`CSR-21` AC2 与 `MP-17` 冲突；`CSR-01` `Release write set` 应为 `N/A`；`CSR-25`/`26` AC 含复合谓词；P2: `CSR-11`/`12`/`23`/`24` 锚点与计数、M1/完成判据、`#1037` `--source=HEAD`/`HEAD^` | R5 全部同步关闭 | Claude/Codex R4 只读评审 |
| R5 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: `CSR-25`/`CSR-26` AC 仍含复合谓词且未按 G-03 排除 ER-04 强制门（Claude R5 遇到 `ECONNRESET` 网络中断） | R6 全部同步关闭 | Codex R5 只读评审 |
| R6 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: 恢复 `CSR-05`/`06` `Release write set: Inherited`；`CSR-25`/`26` 严格按 G-03 仅计卡特有判据（`4/12`, `1/12`）；P2/P3: `CSR-13` `F.t`、`CSR-02`/`03`/`08`/`10`/`19` 拆细、`DEP-CSR-03..05`、exit 128、`#707`/`#716`/`#1037`、家族子卡 `Full-suite trigger: N/A`、`DEFER-CSR-02` 承接位置 | R7 全部同步关闭 | Claude/Codex R6 只读评审 |
| R7 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: `CSR-02`/`08`/`10` 与 `CSR-25`/`26` 仍含多判据合并行（Claude R7 遇到 `ECONNRESET` 网络中断） | R8 全部同步关闭 | Codex R7 只读评审 |
| R8 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: `CSR-02`→`CSR-02,27`、`CSR-08`→`CSR-08,28`、`CSR-13`→`CSR-13,29`、`CSR-20`→`CSR-20,30` 按 G-03 严格拆分；`CSR-01` `6/8`、`CSR-03` 去「等」、`CSR-25`/`26` 补 `libra log -n 2`、`DEP-CSR-05` 接入全部 `restore.rs` 卡、`cli.rs:5329` | R9 全部同步关闭 | Claude/Codex R8 只读评审 |
| R9 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: `CSR-02`+`27`+`03` 设 `REL-CSR-04`（`CSR-32`）、`CSR-08`+`28` 设 `REL-CSR-03`（`CSR-31`）；`CSR-29` `switch -f` 拆为 3 条原子 AC（`6/8`）（Claude R9 遇到 `ECONNRESET` 网络中断） | R10 全部同步关闭 | Codex R9 只读评审 |
| R10 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: `CSR-11` 拆出 `CSR-33`（`6/8` + `3/8`）；拆细 `CSR-04`/`14`；`CSR-29` `switch -f` 指定 `<target-branch>`；精确 `cli.rs` 行号并将 `cli.rs:5320` 移至 `CSR-27`；`CSR-17` 补 `switch_test`；同步 `CSR-13`、`#702`/`#1008` 部分编号、Phase 1/2、`33` 卡与完成判据 | R11 全部同步关闭 | Claude/Codex R10 只读评审 |
| R11 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R11 `FAIL` P1: `CSR-29` `switch -f` 判据仍写 `<current-branch>`（与 `switch.rs:1554` no-op 冲突）；`CSR-33` 将回归验证与 `CSR-11` 变动拆分违反 G-02（Claude R11 `ECONNRESET`） | R12 全部同步关闭 | Codex R11 只读评审 |
| R12 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: `CSR-11`/`CSR-33` 按命令拆分仍割裂共享 `switch.rs`/`restore.rs` 路径（G-02）、`#712` 实测 `checkout -f HEAD~` 场景未覆盖且无依据普通拒绝 AC、`-f` 索引一致与未跟踪保留表述冲突；P2: `CSR-11` 锚点及 `CSR-14`/`29`/`30` 漏 `restore.rs:1941-1997` preflight；P3: R11 日志评审者记录 | R13 全部同步关闭 | Claude/Codex R12 只读评审 |
| R13 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: `CSR-12` 仍依赖已移除的 `CSR-33`；`CSR-11` 合并谓词超 8/8；`CSR-01`/`23` 与 `CSR-09`/`24` 仍拆分实现与互斥校验违反 G-02；P2: 2 处残留 `33` 计数、`CSR-11` 符号链接删除强绑；P3: 4 处源码行号 | R14 全部同步关闭 | Claude/Codex R13 只读评审 |
| R14 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R14 `FAIL` P1: `CSR-09` 含 11/8 复合谓词；`EX-CSR-01`/`02` 不符合门族型验收逐门具名命令与具名审批人条件（Claude R14 `ECONNRESET`） | R15 全部同步关闭 | Codex R14 只读评审 |
| R15 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R15 `FAIL` P1: `CSR-33` 依赖回指 `CSR-34` 成环；`CSR-09`/`CSR-24` 拆分导致缺少 `-p`/`-b`/`-B`/`--orphan` 互斥；`CSR-11`/`CSR-33` 拆分共享安全回归测试 | R16 全部同步关闭 | Codex R15 只读评审 |
| R16 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R16 `FAIL` P1: `CSR-09` 复合 file/stdin 达 9/8；`CSR-31`/`32` 漏全家族守卫；D 组默认未覆盖 `family release point`；`EX-CSR-01` 缺审批证据；`CSR-11`/`05`/`06` 审计表不同步；`CSR-01` 合并 7 个交互键（Claude R16 `ECONNRESET`） | R17 全部同步关闭 | Codex R16 只读评审 |
| R17 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R17 `FAIL` P1: `CSR-01` 未在首卡解禁 D15 且 `y`/`n`/`q` 合并（实 9/8）；`CSR-09`/`33` 拆分引入选项与互斥校验违 G-02；`CSR-11`/`35` 按入口拆分违 G-02；R16 时间戳与 `M1` 计数不一致（Claude R17 `ECONNRESET`） | R18 全部同步关闭 | Codex R17 只读评审 |
| R18 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R18 `FAIL` P1: `CSR-35` 属 `CSR-11` 纯回归测试卡违反 G-02；`CSR-11` 须对 `dir/f` 实施逐组件前导符号链接检查（Claude R18 `ECONNRESET`） | R19 全部同步关闭 | Codex R18 只读评审 |
| R19 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R19 `FAIL` P1: `发布分组与并发窗口` 仍残留 `REL-CSR-06`/`CSR-35`/`CSR-36`；`DEP-CSR-02` 漏 `/Volumes/Sea/libra-backend` 挂载优先核对；`CSR-11` 合并未跟踪与索引致实 `10/8`（Claude R19 `ECONNRESET`） | R20 全部同步关闭 | Codex R19 只读评审 |
| R20 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R20 `FAIL` P1: `EX-CSR-01` 缺恢复审批依据；多卡含复合 AC 与计数偏差（`CSR-05`/`06`/`12` 等）；`DEP-CSR-02` 未接入卡片依赖且需明确身份核对；实现卡缺同卡 ER-06/ER-06a 验收门；P2: 修订历史漏 R20、ER-12 缺具名发布者（Claude R20 `ECONNRESET`） | R21 全部同步关闭 | Codex R20 只读评审 |
| R21 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R21 P1: `EX-CSR-01` 缺外部审批依据、`CSR-33` 独立发布于 `CSR-09` 后；Claude R21 P1: `CSR-09` 复合退出/报错达 15/8；P2: `CSR-03`/`08`/`18`/`20`/`22`/`24`/`30` 复合 AC、`CSR-25` VER 5/12、追溯表/风险表/`split-from`/具名发布者/`cf` 推送时序/`CSR-21` AC5 | R22 全部同步关闭 | Claude & Codex R21 只读评审 |
| R22 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Claude P1: 卡数/发布点、`EX-CSR-01` 残留、追溯表、`CSR-11` 覆盖、家族 `cf` 推送、复合 AC；Codex P1: `CSR-09`/`CSR-35` 违反 G-02、文档未逐文件、追溯/M1、评审结论 | R23 全部同步关闭 | Claude & Codex R22 只读评审 |
| R23 (Claude claude-opus-5-5 + Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R23 `FAIL` P1: 残留完整 `CSR-35` 卡、`CSR-34` 测试名与 `CSR-09` 不一致、`CSR-17` 审计 VER（Claude R23 `ECONNRESET`） | R24 全部同步关闭 | Codex R23 只读评审 |
| R24 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | 见 R24 原始日志；本行只纠正「待复审」占位，不把占位当结论 | 后续修订 | Codex R24 只读评审 |
| R27 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R27 FAIL P1: 全局「每张卡完成后」与家族子卡不发布矛盾；D 组未列 job；`CSR-33`/`CSR-24` 文档 AC 漏网站文件本地更新（Claude 未跑） | `2026-11-17 09:21:13 UTC` 修订关闭 | Codex R27 只读评审 |
| R29 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R29 FAIL P1: D 组要求 PR 与 `main` push 使用同一个 head SHA（Claude 未跑） | `2026-11-17 10:43:14 UTC` 拆开 PR head 与 merge SHA | Codex R29 只读评审 |
| R30 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R30 FAIL P1: `CSR-11` VER 少计；`CSR-13` 缺 status 文档；文档影响字段使用缩写（Claude 未跑） | `2026-11-17 10:52:35 UTC` 关闭 | Codex R30 只读评审 |
| R31 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R31 FAIL P1: 追溯表 `_compatibility.md` 与 `status` 文档未同步（Claude 未跑） | `2026-11-17 11:14:03 UTC` 关闭 | Codex R31 只读评审 |
| R32 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R32 FAIL P1: D 组未检查 CDN 产物可下载（Claude 未跑） | `2026-11-17 11:19:54 UTC` 关闭 | Codex R32 只读评审 |
| R35 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: D 组遗漏 Windows 签名清单使用的无后缀产物，且 HEAD 不能证明完整下载；已补无后缀对象及五个二进制完整 GET、大小和 SHA-256 校验。该轮明确未完成全文复审，R36 不得据此推定其它维度通过 | R36 完整复审 | Codex R35 只读评审（部分覆盖） |
| R36 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | P1: M0 仍要求 Claude+Codex PASS；已改为 Codex PASS。该轮未完成全文复审，R37 不得据此推定其它维度通过 | R37 完整复审 | Codex R36 只读评审（部分覆盖） |
| R37 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | 无新增 P1；合并输出截断导致全文复审未完成。已确认 M0 与 CDN 修复保留，R38 改按任务卡区间复审 | R38 分区完整复审 | Codex R37 只读评审（部分覆盖） |
| R38-A (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` 第 1–1000 行 | FAIL | P1: `.size` 不由 release 上传；P2: `restore` 已有 stdin 读取器、跨计划漏 `CSR-23`/`CSR-24`。已改用签名 manifest 的 size/SHA、复用现有读取器并补齐写集。R39-A 分区复核 | R39-A 分区完整复审 | Codex R38-A 只读评审（分区覆盖） |
| R39-A (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` 第 1–1000 行 | FAIL | P1: 原合同要求 manifest 含第五条 `.exe` 产物，实际仅四平台且 Windows 仅无后缀 URL。已改为无后缀对象先匹配条目、`.exe` 与该条目比较 size/SHA，不要求第五条记录。R40-A 分区复核 | R40-A 分区完整复审 | Codex R39-A 只读评审（分区覆盖） |
| R40-A (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` 第 1–1000 行 | FAIL | P1: `CSR-27` 仅要求接受 patch 辅助选项，未验证实际行为。已增加 hunk 边界和 auto-advance 交互影响断言（AC=7/8，VER=3/8 不变）。R41-A 分区复核 | R41-A 分区完整复审 | Codex R40-A 只读评审（分区覆盖） |
| R41-A (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` 第 1–1000 行 | FAIL | P1: hunk 测试只要求输出变化，无法判定 Git 边界。已固定第 5/15 行修改及精确上下文、合并/分裂和最终文件断言。R42-A 分区复核 | R42-A 分区完整复审 | Codex R41-A 只读评审（分区覆盖） |
| R43-A1 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` 第 1–500 行 | FAIL | P2: `-U=2`/`-U=0` 短选项拼写与 Git 不兼容。已改为 `-U2`/`-U0` 并要求以目标 Git 验证 `checkout -p` 参数解析。R44-A1 分区复核 | R44-A1 分区完整复审 | Codex R43-A1 只读评审（分区覆盖） |
| R44-A1 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` 第 1–500 行及定向交叉核对 | PASS | 无 P0/P1/P2。`-U2`/`-U0`、固定 hunk 与最终文件断言、CDN manifest 双 Windows 对象、CSR-33 与 DEP-WC-04 均符合 | R44-A2 复审第 501–1000 行 | Codex R44-A1 只读评审（分区覆盖） |
| R44-A2 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` 第 501–1000 行及定向交叉核对 | FAIL | P1: `pathspec_from_file_B_*` 测试名触发 `non_snake_case`。已改为 `pathspec_from_file_force_branch_*` 并同步命令和判据规范。R45-A2 分区复核 | R45-A2 分区完整复审 | Codex R44-A2 只读评审（分区覆盖） |
| R45-A2 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` 第 501–1000 行、CSR-27/CSR-11/CDN manifest 定向复核 | PASS | 无 P0/P1/P2。`pathspec_from_file_force_branch_*` 测试名与 Verification/判据命令一致 | R46-B 复审第 1001 行至文件末 | Codex R45-A2 只读评审（分区覆盖） |
| R46-B (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` 第 1001–1774 行及 CSR-27/CDN manifest 定向复核 | FAIL | P1: `CSR-12` 缺合并失败回滚验收/测试，`CSR-18` 缺共享预检回归及冲突状态断言。已分别补 `merge_checkout_failure_rolls_back` 与 `checkout_b_and_switch_c_conflict_preserves_state`。R47-B 分区复核 | R47-B 分区完整复审 | Codex R46-B 只读评审（分区覆盖） |
| R47-B (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` 第 1001–1779 行及 CSR-27/CDN manifest 定向复核 | FAIL | P1: `CSR-18` 新增 switch 测试未写入实现写集；P2: 两个新增测试未进入追溯表。已补测试文件与两条追溯。R48-B 分区复核 | R48-B 分区完整复审 | Codex R47-B 只读评审（分区覆盖） |
| R48-B (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` 第 1001–1781 行及 CSR-27/CDN manifest 定向复核 | PASS | 无 P0/P1/P2。CSR-18 写集、CSR-12/CSR-18 追溯、故障回滚、共享预检回归及计数均符合；与 R44-A1/R45-A2 共同覆盖完整计划 | 提交并推送该计划 | Codex R48-B 只读评审（分区覆盖） |

## 实施证据汇总

- 计划成稿时间：`2026-11-17 03:03:00 UTC`；最新修订时间：`2026-11-17 12:27:28 UTC`；仓库基线 `main@62abb622f0eb2626934633ada78061963ebf1f44`；`Cargo.toml` 版本 `0.30.207`。
- 后续各卡的 focused 测试计数、nextest 全量摘要、`release.yml` run ID 与 D 组结果将直接登记于本节。

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-CSR-01 | 无（20 个分配 issue 已全量进入 `CSR-01`..`CSR-25`、`CSR-27`..`CSR-34`（`CSR-35` 已废弃）） | N/A | N/A | N/A |
| DEFER-CSR-02 | `#705` delayed-filter 进度输出、`#714`/`#715` parallel-checkout worker 计数、以及外部上游完整 `t*.sh` / `TC-xxxx` 脚本全量跑通 | 依赖尚未落地的并行检出/过滤子系统与外部全局 CLI 前置 | 上游并行检出/过滤引擎合入后，由 CLI / Git-compat 维护者执行完整上游回归 | 后续并行检出与上游兼容性收敛计划（Owner: 仓库 CLI 维护者） |

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
- [ ] 每张独立发布卡与家族发布点（`CSR-25`、`CSR-31`、`CSR-32`、`CSR-34`）均已完成 `patch + 1`、版本面三处同步、工具链刷新 `Cargo.lock`、`gh release create v<version>` 触发 `release.yml` 并取得 Cloudflare 发布证据（ER-08）。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
- [ ] `plan-long.md` 相关状态或日期计划索引已同步，或明确 `N/A`。
