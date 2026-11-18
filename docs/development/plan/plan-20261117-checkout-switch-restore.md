# `libra checkout` / `switch` / `restore` Git 兼容与缺陷修复计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：Claude R11 与 Codex R11（A–E 五分区）双审 PASS（受审 SHA-256 前 12 位 `fd3f96ccd547`），计划可提交；全部任务 `pending`，尚未执行。最近修订 `2026-11-18 18:41:45 UTC`（仅记账）。本计划不在 v2.16 `code-only` 具名范围内，执行标准 `patch + 1` + `gh release create` 发布合同。**计划文本提交门**：按用户 2026-11-18 指令，为 Claude（Subagent）与 Codex 双审，二者对同一字节均给出 `VERDICT: PASS` 方可提交（取代 2026-11-17「Codex 单审、不再要求 Claude Review」口径；执行期各卡 ER-05 评审门与网站 Claude Code PASS 门不变）。
> 网站交付路径（已确认）：唯一网站检出为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支，`.libra` 仓库、无 `.git`），依据为用户 2026-11-18 11:55:09 UTC 确认；仓库 `AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`（本机不存在），属已知文档滞后，本计划不修改该文件（`DEP-CSR-02`）。
> 覆盖 Open Issues（20 个）：`#702`、`#703`、`#704`、`#705`、`#706`、`#707`、`#708`、`#709`、`#712`、`#713`、`#714`、`#715`、`#716`、`#718`、`#721`、`#723`、`#724`、`#1008`、`#1030`、`#1037`。

## 文档职责

本文解决 `libra checkout`、`libra switch`、`libra restore` 及关联 D/F 合并路径（`src/command/merge.rs`）与状态检查（`src/command/status.rs`）在 Git 兼容测试中的 20 项已知缺陷，目标是逐卡交付可独立验收、可独立发布、附带单元/集成测试与中英文/网站文档同步的修复切片。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收。

### 适用范围

- `libra restore` 与 `libra checkout` 的 `-p`/`--patch` 交互模式（复用 `src/internal/patch_mode/` 并移除 `src/cli.rs:3028-3070` D15 拒绝表中 `restore -p`、`checkout -p` 与 `checkout --[no-]auto-advance` 条目，同步 `tests/compat/compat_declined_interactive_flags.rs` 与 `docs/development/commands/_compatibility.md`）。
- `libra checkout` 的 `--overlay`/`--no-overlay`、`-2`/`--ours`/`-3`/`--theirs`、`<tag>` 源树、`.` 路径消歧、`--detach`+`-b` 互斥、追踪状态、`--pathspec-from-file`、`--track=direct|inherit`、`-f` 符号链接安全、`-m` 合并切换、`status.rs` D/F 检查、前导目录符号链接、`--orphan <start-point>`、unborn `-b`、DWIM 多远端/单远端、`-B` 保留暂存与非空目录替换。
- `libra merge` 的 D/F 冲突阶段记录（`destdir~HEAD` stage 1/2，`src/command/merge.rs:4031`、`src/command/merge/rename_merge.rs`）。
- `libra switch` 在 unborn HEAD 下的 `-C <branch>` 与共享工作区/DWIM 逻辑（`src/command/switch.rs`）。
- 对应测试（`tests/command/{checkout_test,switch_test,restore_test,merge_test}.rs`，均编译在 `--test command_test`）、命令/开发/网站文档（`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/`）、`COMPATIBILITY.md` 与 `docs/error-codes.md`。

### 非目标

- 不更改既有 `checkout --ignore-other-worktrees` 的有意设计差异（Libra 禁止同一分支在多个 worktree 同时检出）。
- 不在本计划中修改 `branch` 或 `grep` 命令自身缺口（由同批并行计划承接）。
- 不引入新的 SQLite schema 迁移或修改 `GlobalConfig` / `SystemConfig` 表结构。

### 成功定义

- 20 个指定 GitHub Issue 对应的全部行为缺口在代码中修复，并有确定性 focused 测试覆盖正常路径与失败路径。
- 每个修改用户可见行为的任务卡同步更新对应命令的 EN/zh-CN 文档、开发文档、网站文档及 `COMPATIBILITY.md`。
- 全部 38 张活动任务卡（`CSR-35` 已合回 `CSR-09`，编号保留为废弃记录；含 6 张家族发布点 `CSR-25`/`CSR-31`/`CSR-32`/`CSR-34`/`CSR-10R`/`CSR-15R`）达到 `Lifecycle=done` 且 `Acceptance=complete`，全部卡与收口全量门均通过；计划文本以 Claude（Subagent）与 Codex 对同一字节双审 `VERDICT: PASS` 为提交门（见头部）。

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
| 文档 | `docs/commands/{checkout,switch,restore,merge}.md`（EN+zh-CN）、`docs/development/commands/{checkout,switch,restore,merge}.md`、`_compatibility.md`、网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/`、`COMPATIBILITY.md` | `COMPATIBILITY.md:190,273` |
| 测试 | 集成测试模块 `checkout_test`、`switch_test`、`restore_test`、`merge_test` 注册于 `tests/command/mod.rs`，由 `--test command_test` 运行 | `tests/command/mod.rs:1020,1124,1150` |
| 外部参照 | Git `2.56.0` @ `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（`t/t2012`、`t/t2020`、`t/t2024`、`t/t2025`、`t/t2060`、`t/t2070`、`t/t3404`、`t/t1008` 等）；核对日期 2026-11-17 | `gh issue view`（#702..#724, #1008, #1030, #1037） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-CSR-01 | `restore -p/--patch` 在 `src/cli.rs:3028-3048` 被 D15 拒绝，未接入 `src/internal/patch_mode/` | `#1037` 交互式 hunk 恢复不可用 | `src/cli.rs:3028-3048`、`src/internal/patch_mode/` | `CSR-01` |
| GAP-CSR-02 | `checkout -p/--patch` 在 `src/cli.rs:3040` 被 D15 拒绝，未复用 `patch_mode` | `#702`（部分 1/4）patch 检出不可用 | `src/cli.rs:3040`、`src/command/checkout.rs:53-120` | `CSR-02` |
| GAP-CSR-03 | `checkout` 委托 `restore` 默认采用 `--no-overlay`，缺少 `--overlay` 默认与 `-p --overlay` 互斥报错 | `#702`（部分 4/4）默认语义与冲突信息不兼容 | `src/command/checkout.rs:102-107,720-722` | `CSR-03` |
| GAP-CSR-04 | `CheckoutArgs` 未注册 `--theirs` / `--ours`（含 `--theirs --no-overlay` 删文件） | `#703` 无法在 `checkout` 选边解决冲突 | `src/command/checkout.rs:53-120` | `CSR-04` |
| GAP-CSR-05 | `checkout --no-overlay <tag> -- <path>` 未正确解析 tag 为源树 | `#704` tag 源检出失败 | `src/command/checkout.rs:706-757` | `CSR-05` |
| GAP-CSR-06 | `index_matches_path` 对 `.` 不匹配且未覆盖 `--no-progress`/`--quiet --progress` | `#705` `checkout --no-progress .` 误入切分支路径 | `src/command/checkout.rs:695-704` | `CSR-06` |
| GAP-CSR-07 | `CheckoutArgs` 允许 `--detach` 与 `-b`/`-B`/`--orphan` 联用 | `#706` 违反 Git 互斥契约 | `src/command/checkout.rs:57-77` | `CSR-07` |
| GAP-CSR-08 | `checkout` 切换分支后未打印 ahead/behind 追踪状态行（含本地 `.` 上游） | `#707` 缺少上游追踪提示 | `src/command/checkout.rs:1229-1286` | `CSR-08` |
| GAP-CSR-09 | `CheckoutArgs` 缺少 `--pathspec-from-file`/`--pathspec-file-nul` 及互斥报错（含 `--detach`） | `#708` 批量路径文件输入不可用 | `src/command/checkout.rs:53-120` | `CSR-09` |
| GAP-CSR-10 | `CheckoutArgs.track` 为 `bool`，不支持 `--track=inherit`/`direct` 与 `autoSetupMerge=inherit` | `#709` 追踪继承模式失效 | `src/command/checkout.rs:84-85,899-914` | `CSR-10` |
| GAP-CSR-11 | `checkout -f` / `switch -f` 删除条目时跟随前导符号链接误删目标目录内容 | `#712` 安全/数据丢失风险 | `src/command/restore.rs:684,764,2045-2056`（另 `:2164,2206` 同类） | `CSR-11` |
| GAP-CSR-12 | `CheckoutArgs` 拒绝 `-m`/`--merge`，缺少三方合并与后续 `checkout -f` 消解冲突 | `#713` 无法携带本地修改合并切换 | `src/command/checkout.rs:53-120` | `CSR-12` |
| GAP-CSR-13 | `status.rs:745` 在跟踪目录被文件替换后触发 `LBR-IO-001`；普通 `checkout HEAD` 应保留用户修改，仅 `--force HEAD` 恢复 | `#714` 状态预检误报错 | `src/command/status.rs:745`、`src/command/checkout.rs` | `CSR-13` |
| GAP-CSR-14 | 普通切换写入链 `restore_target_to_file_typed` 不检查前导目录符号链接：`--force` 不以真实目录替换该链接，非 force 时可能穿过链接写入外部目录 | `#715` 强制切换无法替换目录链接 | `src/command/restore.rs:684,1817-1840`（`:1235-1260` 只用于显式目标） | `CSR-14` |
| GAP-CSR-15 | `run_checkout` 显式拒绝 `checkout --orphan <new-branch> <start-point>` | `#716` 不支持 orphan 起点参数 | `src/command/checkout.rs:461-470` | `CSR-15` |
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
| 同批写 `src/cli.rs` 的计划（`commit-signing`、`worktree-clean-reset-apply`、`global-init-infra`，以及 `branch`/`clone`/`fetch-push`/`plumbing-misc`） | `src/cli.rs` 串行窗口；本计划写者为 `CSR-01`、`CSR-23`（`REL-CSR-01`）与 `CSR-02`、`CSR-27`（`REL-CSR-04`） | `DEP-CSR-03`（对方 `DEP-CS-05`、`DEP-WC-07`、`DEP-GII-03` 按 ID 引用） |
| `plan-20261117-commit-signing.md`（已推送 `2fe8c4b`） | 对方 `DEP-CS-05` 要求本计划 `CSR-01`、`CSR-02`、`CSR-27` 的 `src/cli.rs` 修改合入 `main` 后才开工 `CS-08`；对方 `CS-02a`/`CS-03c`/`CS-03d`/`CS-03g`/`CS-03h`/`CS-03i`/`CS-03k`/`CS-03l` 写 `src/command/merge.rs` 或 `docs/commands/merge.md`，与 `CSR-21` 相交 | `DEP-CSR-10`（三卡均为家族子卡，合入 `main` 的时点为 `CSR-25`、`CSR-32` 的 squash merge）；`CSR-21` 经 `DEP-CSR-09` |
| `plan-20261117-plumbing-misc.md`（已推送 `cd5ccac`） | 对方 `DEP-PM-15`：`PM-21a` 与本计划写 `checkout.rs`/checkout 文档/网站 `checkout.en.md` 的卡互斥（独占写锁，不设先后顺序边） | `DEP-CSR-08`（对等互斥锁） |
| `plan-20261117-global-init-infra.md`（已推送 `f3316d9`） | `GII-23` 写 `src/command/checkout.rs`、checkout 文档与网站页，`GII-21` 写 checkout/merge 文档与网站页；`GII-10`/`GII-11` 写 `src/command/status.rs`、`src/command/status/output.rs` 与 status 文档/网站页（与 `CSR-08`、`CSR-13` 相交）；`GII-13`/`GII-21` 写 merge 文档/网站页与 `tests/command/merge_test.rs`（与 `CSR-21` 相交）；对方 `DEP-GII-03` 亦覆盖 `COMPATIBILITY.md`、`src/cli.rs` 与网站生成产物 | `DEP-CSR-08`（checkout 文件）、`DEP-CSR-09`（其余交集与生成产物）、`DEP-CSR-07`、`DEP-CSR-03` |
| `plan-20261117-worktree-clean-reset-apply.md` | 对方 `DEP-WC-04` 登记本计划 14 张写 `restore.rs` 的卡（`CSR-01, 05, 06, 09, 11, 12, 13, 14, 20, 23, 24, 29, 30, 33`，与本计划写集一致）；`DEP-WC-07` 登记 `CSR-01, 02, 23, 27` 为 `src/cli.rs` 写者（与本计划一致） | `DEP-CSR-05`、`DEP-CSR-03` |
| `plan-20261117-diff.md`（已推送 `ca03182`） | `DIFF-09` 在 `src/command/diff.rs` 实现 `--inter-hunk-context` 的 hunk 合并；`CSR-27a` 复用该实现 | `DEP-CSR-12`；`src/command/diff.rs` 交集经 `DEP-CSR-09` |
| `plan-20261117-fetch-push.md` | 对方 `DEP-FP-03`（outgoing）把 `#719` 验收 3 / `#676` 的 `checkout -b new origin` 未出生分支检出移交本计划 | 本计划 20 个 issue 不含 `#676`/`#719`，未承接：`DEP-CSR-11` 与 `DEFER-CSR-03`，按对方回落口径 `#719` 保持 OPEN |
| `plan-20261117-merge-pull.md`、`plan-20261117-branch.md` | 共享 `merge.rs`/`rename_merge.rs`、`branch.rs`（对方计划尚未提交至 `main`，按 ID 引用、以其提交版本为准） | `DEP-CSR-04`、`DEP-CSR-06` |
| 同批全部 `plan-20261117-*.md` | 共享 `COMPATIBILITY.md`、`docs/error-codes.md` 与网站生成产物 `docs-manifest.ts`/`docs-search.ts`/`dist/` | `DEP-CSR-07`、`DEP-CSR-09` |
| `plan-status.md` | 全仓计划单一视图 | 本计划建稿由协调者统一登记；执行期每卡推进同批更新 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 20 个 `checkout`/`switch`/`restore`/`merge` 兼容缺陷覆盖 patch、合并检出、DWIM、D/F 冲突与符号链接安全，值得逐轴收敛 | 无 |
| 可行性 | 落点集中在 `checkout.rs`、`switch.rs`、`restore.rs`、`status.rs`、`merge.rs`、`cli.rs` | 拆分为 38 张活动小卡（含 6 张家族发布点 `CSR-25`/`CSR-31`/`CSR-32`/`CSR-34`/`CSR-10R`/`CSR-15R`；`CSR-35` 已合回 `CSR-09`） |
| 任务卡粒度 | `G-03` 按独立谓词计数；失败路径以稳定错误码为单一谓词（ADR-CSR-03），`CSR-09` 由此收敛为 8/8 并撤销 `EX-CSR-03`；`CSR-27`/`27a`、`CSR-10`/`10a`、`CSR-15`/`15a` 按行为轴拆分；全部活动实现卡 AC/VER≤8、发布点≤12 | 全部活动卡为 `M` 或 `S`，无 waiver |
| 依赖与顺序 | 共享写集严格串行 `CSR-01 -> ... -> CSR-22`；跨计划经 `DEP-CSR-03`..`DEP-CSR-12` 按 ID 协调 | 无并发 I–I 冲突 |
| 完整性 | 修正 `CSR-13`（保留修改 + `status.rs:745`）、`CSR-01`/`02`（复用 `internal/patch_mode` + `cli.rs` D15）、`CSR-21`（`merge.rs` stage 1/2）、`CSR-16`（unborn `-b` 成功）、`CSR-03`（overlay 默认）、`CSR-06`（`.` + `--no-progress`）、`CSR-09`（`--detach` + 共享写集 `T-1`）、`CSR-18`（`switch.rs:852` `T-1`） | 无遗漏 |
| 安全性 | `CSR-11`/`CSR-14` 严格使用 `symlink_metadata`；`CSR-13` 普通 `checkout HEAD` 绝不删除用户替换文件 | 在对应卡明确 |
| 功能正确性 | 对齐 Git `6de20f6092` 语义、stage 1/2 与 Issue 要求的诊断文案；期望值取自同夹具 pinned Git（「验证执行协议」第 4 项），失败路径以稳定错误码判定（ADR-CSR-03） | 已写入每卡 AC 与断言映射 |
| 接口兼容 | `Rollback mode` 统一为 `immutable-release` 并附降级指引与兼容窗口 | 已登记 |
| 数据流与控制流 | `CSR-01`/`02` 复用既有 `src/internal/patch_mode/`（GC-02） | 无重复引擎 |
| 性能与容量 | 继承 GC-10 | 无无界扫描 |
| 可靠性与容错 | 禁止新增裸 `unwrap()`/`expect()` | 继承 GC-11 |
| 可维护性 | 公共路径/合并/patch 逻辑收敛在既有共享模块 | 已分层 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
| 2026-11-18 18:07:36 UTC | 2026-11-18 双审 R1：Claude FAIL（7×P1）、Codex FAIL（A–E 五分区共 15×P1），受审 SHA-256 `8dad1040f10e…` | ① 「验证执行协议」：Git 侧 `S.wt` 排除 `.git`；Git 期望改以 `git archive` 只读取得源码构建；新增 4(e) 夹具复用规则与「结果状态等于 Git 期望」单一谓词定义；副作用边界改为失败路径 `S` 整体不变、成功路径 `S.cfg`/`S.obj` 不变且其余分量以映射断言为准（成功切换的 `HEAD`/reflog 变化不再误判）；② 家族发布点文档哈希门改为「子卡登记值等于该子卡提交 + 每个文件最后修改者登记值等于发布树」；③ `CSR-02` 补非 `HEAD` 源 `checkout -p C0`（AC 7/8）并修正会把三处修改合成一个 hunk 的 AC2 夹具；`CSR-23` merge-base 改用分叉分支 `side...`；`CSR-27` 交互改为全答 `n` 并断言 `S` 不变；④ `CSR-08` 把共享追踪段渲染对齐 pinned Git（落后行 `, and can be fast-forwarded.`、分叉提示），`libra status` 一并对齐并新增 2 条 AC（8/8），写集补 `src/command/status/output.rs`、`tests/command/status_test.rs` 与 status 文档/网站页，`CSR-31` 网站页补 `status.en.md`；⑤ `CSR-11` checkout 场景改为 `checkout -f HEAD~1` 以覆盖 `checkout_to_commit`；`CSR-14` 目标分支改写 `dir/file` 以区分是否穿过符号链接写入；⑥ `CSR-17` switch 拆出稳定码 AC（5/8）；`CSR-18` 改为引用重置、`HEAD`、整体结果状态三条，干净树回归归共享回归组；`CSR-19` 拆出 `HEAD` AC 并以「远端前进后远程追踪引用不变」验证不访问网络（7/8）；`CSR-12`/`16`/`22` 复合 AC 收窄；`CSR-06`/`13`/`29` 删去映射中超出 AC 的附加谓词；`CSR-33` 守卫加强为不得直接读取文件或标准输入；测试矩阵正则改为逐模块命令 | 无拆卡；`CSR-02`、`CSR-08`、`CSR-17`、`CSR-19` AC 增加 | 「验证执行协议」、相关任务卡、审计表、追溯表、测试矩阵、review log |
| 2026-11-18 18:15:28 UTC | 2026-11-18 双审 R2：Claude FAIL（1×P1）、Codex FAIL（A–E 五分区共 14×P1），受审 SHA-256 `323cf204dfa4…` | ① 「验证执行协议」：新增引用断言口径（按 `reference` 表 `kind`/`remote`/`name` 判定，对应 `src/internal/head.rs` 存储短名）、路径恢复副作用边界（索引源只写工作区时 `S.idx` 不变、`--staged` 时 `S.wt` 不变）、成功创建带上游分支时 `branch.<新分支>.*` 配置等于 Git 期望、「冲突结束」不适用 `S` 不变；失败文案仅在 Issue 验收要求时单列（ADR-CSR-03 列明清单）；② 发布点 `CSR-25`/`CSR-32`/`CSR-34`/`CSR-10R` 的 Dependencies 补其持有至合入的跨计划窗口（`DEP-CSR-03`/`05`/`06`）；③ `CSR-03` 补 `--no-overlay -b` 拒绝（8/8）；`CSR-06` 改为整体结果状态与不进入分支切换；`CSR-07` 删除 #706 未要求的诊断文案 AC（4/8）；`CSR-08` status 段比较到首个空行（含计数行）；④ `CSR-09` 按 #708 验收补 `--detach`/`--patch`/位置 pathspec 三条诊断文案 AC，`-B`/`--orphan`（同一路径模式守卫）与缺失文件移出卡特有 AC（8/8），引号路径登记 `DEFER-CSR-04`；`CSR-24` 补 `--pathspec-file-nul` 诊断 AC（4/8）；⑤ `CSR-14` 改为三入口各自「结果状态等于 Git」与「外部目录不变」（含 `switch -f`，7/8）；`CSR-18` 按 #723 改为重置当前分支（5/8）；`CSR-19` 双远端夹具改挂 AC4、AC3 写明同一次调用、「不访问远端」改为可观测的「不执行 pull/fetch」；`CSR-21` 补冲突结束稳定码 AC（5/8） | 无拆卡；AC 计数随之更新 | 「验证执行协议」、ADR-CSR-03、相关任务卡、审计表、追溯表、延后项、review log |
| 2026-11-18 18:18:53 UTC | 2026-11-18 双审 R3：Claude FAIL（1×P1）、Codex FAIL（A 区 PASS；B–E 共 6×P1），受审 SHA-256 `490681c33c90…` | ① 家族发布点文档哈希门拆为「子卡登记值等于子卡提交」与「发布树等于最后修改者登记值」两条独立 AC/V（发布点 AC/VER 各 +1）；② `CSR-03` 夹具源改为 C1 完整对象名，不再依赖 `CSR-05` 才交付的 tag 解析；③ `CSR-33` 删去无法证明的源码零命中守卫，「不新增第二套读取实现」改为由 ER-05 review 核对的实现约束（2/8，VER 1/8），`CSR-34` 聚合守卫同步；`CSR-33` 映射不再引用其它卡；④ `CSR-12` AC6 明确在 AC3 冲突调用之后执行并前置断言 stage 1/2/3；`CSR-13` 保留断言改为调用前后 `S.wt`、`S.idx` 整体不变（5/8）；`CSR-18` 补「`HEAD` 仍指向当前分支」AC（6/8）；`CSR-22` 夹具先暂存 `a.txt` 使「保留索引」可判定；⑤ 「兼容与文档收口」补 `status`（Codex A 区 P2） | 无拆卡 | 相关任务卡、发布点、审计表、追溯表、兼容收口、review log |
| 2026-11-18 18:22:30 UTC | 2026-11-18 双审 R4：Claude FAIL（1×P1）、Codex FAIL（B 区 PASS；A/C/D/E 共 7×P1），受审 SHA-256 `82f0b6f34021…` | ① 网站第 8 步家族口径改为「每个页面以家族内最后修改它的子卡登记值为准」（`REL-CSR-03` 的 `status.en.md` 由 `CSR-08` 最后修改）；`DEP-CSR-07` 允许原地修改的 `COMPATIBILITY.md` 行补 `status`；② `CSR-10a` 拆出上游配置 AC（3/8）；`CSR-15` AC4 明确在 AC1 调用之后提交；`CSR-29` 改为两入口各自「结果状态等于 Git」与 `HEAD` 断言（4/8），并按源码更正根因证据（`NonEmptyWorktreeDirectory` 只拦截真实非空目录；Issue #714 的失败文案来自 `checkout.rs:233-236`），`CSR-14` 证据同步删去不成立的预检说法；③ `CSR-20` 两个成功场景改为完整结果状态比较并加入无关未跟踪文件（4/8）；`CSR-19` 去掉无法观测的「不访问远端」表述，改为「以本地远程追踪引用为准」与 `pulled=false`；「发布窗口顺序」只列发版单元 | 无拆卡 | 网站第 8 步、`DEP-CSR-07`、相关任务卡、审计表、追溯表、review log |
| 2026-11-18 18:25:09 UTC | 2026-11-18 双审 R5：Claude FAIL（5×P1）、Codex FAIL（B、D 区 PASS；A/C/E 共 4×P1），受审 SHA-256 `999b093fcaa7…` | ① `S.idx` 改为读取 `.libra/index` 条目并比较 mode、对象名、stage、路径与 `assume_valid`/`skip_worktree`/`intent_to_add` 标志（不比较 stat 缓存），使「索引不变」能发现标志丢失；② `CSR-08` 写集补 `src/command/status.rs`（`mod output` 私有，需 `pub(crate)` 导出渲染入口）；③ 切换类成功场景统一补 `HEAD` 目标与完整结果状态：`CSR-11` 两入口各为外部目录不变、结果状态等于 Git、`HEAD` 断言（6/8）；`CSR-14` 删去 #715 未要求的 `checkout --force t` 场景，`switch -f t` 补 `HEAD`（6/8）；`CSR-12` AC2 改为整体结果状态；`CSR-19` 补 AC1 场景结果状态（8/8）；`CSR-20`、`CSR-30` 补 `HEAD` 断言（各 6/8），`CSR-30` 结果状态含索引 | 无拆卡 | 「验证执行协议」第 6 项、相关任务卡、审计表、追溯表、review log |
| 2026-11-18 18:28:38 UTC | 2026-11-18 双审 R6：Claude PASS、Codex FAIL（A、B 区 PASS；C/D/E 共 5×P1），受审 SHA-256 `f7075bda1ea3…` | ① `CSR-04` 选边写出补 `-2`/`-3` 短选项各一条，拒绝路径按 Git 两个 die 分支各取一例（`--ours HEAD`、`--theirs -b`），其余同分支组合写入 Out of scope（7/8）；② `CSR-07` 夹具令 `topic` 与当前 `main` 不同，AC4 改为「`HEAD`、工作区与索引」结果状态等于 Git；③ `CSR-28` 夹具令 `topic` 树与 `main` 不同，并为 `--detach`/`--quiet`/`--json` 三次调用各补「结果状态等于 Git」AC（7/8）；④ `CSR-29` Description 删去与根因证据矛盾的「预检在 `--force` 下不再拦截」，明确修复按复现定位的失败入口且不放宽非空目录与子模块保护；⑤ `CSR-19` 夹具写明 R 与 U1 的树不同（切换须删 `r.txt`、写 `f.txt`） | 无拆卡 | `CSR-04`、`CSR-07`、`CSR-19`、`CSR-28`、`CSR-29`、审计表、追溯表、review log |
| 2026-11-18 18:31:07 UTC | 2026-11-18 双审 R7：Claude PASS、Codex FAIL（A、B 区 PASS；C/D/E 共 3×P1），受审 SHA-256 `1ca93d383ed3…` | ① F08 写全各提交的树、文件字节与「建分支 → 提交 → 返回 `main`」顺序（R 的 `main` 为与 O 无关的 Cr，本地上游改为 `base0`），`CSR-28` 夹具随之说明 `topic` 与 `main` 树不同；② `CSR-14` 两个成功场景改为工作区与索引整体比较 Git 期望（`switch -f t` 索引须记录 `F2`）；③ 按 G-09 在九张原卡（`CSR-01`、`CSR-02`、`CSR-27`、`CSR-08`、`CSR-09`、`CSR-10`、`CSR-13`、`CSR-15`、`CSR-20`）的 Out of scope 补「本卡拆出 …」登记，与新卡 `split-from` 双向一致；④ 同类清扫：F10、F17 补全远端与本地仓库的树和构造步骤 | 无拆卡 | `CSR-08`、`CSR-10`、`CSR-14`、`CSR-17`、`CSR-28` 与九张原卡、review log |
| 2026-11-18 18:34:15 UTC | 2026-11-18 双审 R8：Claude PASS、Codex FAIL（A、E 区 PASS；B/C/D 共 3×P1），受审 SHA-256 `96e19f32ff81…` | ① 「验证执行协议」第 6 项新增「切换结果门」全局门：卡特有用例中每个成功的分支切换/创建类调用之后，`HEAD` 指向及其提交、`S.wt`、`S.idx` 必须等于同步骤 Git 夹具结果（一次性覆盖 `CSR-08` 各输出场景、`CSR-28` 缺上游场景、`CSR-10`/`CSR-10a` 各建分支场景等只断言输出或配置的调用）；② `CSR-27a` 的 hunk 头提取改为匹配正则子串、不要求行首（管道 stdin 下提示不换行，`add-patch.c:1678-1687,1745-1761`、`prompt.c:79-82`）；`CSR-17` AC1 措辞去掉「且」 | 无拆卡 | 「验证执行协议」第 6 项、`CSR-17`、`CSR-27a`、review log |
| 2026-11-18 18:36:58 UTC | 2026-11-18 双审 R9：Claude PASS、Codex FAIL（A、C、D、E 区 PASS；B 区 1×P1），受审 SHA-256 `70f2daed4e6b…` | ① `CSR-27` 交互改为两文件各答一次 `y` 并含 `>` 导航，新增两条 AC 断言 `--no-auto-advance`（会话结束时统一应用）与 `--auto-advance` 各自的最终 `a.txt`/`b.txt` 字节等于 Git（7/8）；② `CSR-06` 补从子目录 `sub/` 执行 `checkout --no-progress .` 的范围断言（C 区 P2，5/8） | 无拆卡 | `CSR-06`、`CSR-27`、审计表、追溯表、review log |
| 2026-11-18 18:39:35 UTC | 2026-11-18 双审 R10：Claude FAIL（1×P1）、Codex FAIL（A、B、C、E 区 PASS；D 区 2×P1），受审 SHA-256 `5b08ed22500f…` | ① 按源码更正 `CSR-11` 根因：两个入口实际经 `restore_worktree_tracked`（`restore.rs:684`）在 `:764` 调用 `remove_worktree_path_for_restore`（`:2045-2056`，只对整串路径做 `symlink_metadata`）删除，`:2206` 属另一函数 `restore_worktree`（`:2164`），两处复用同一逐组件检查；标题、Description、证据、GAP-CSR-11、追溯表同步；② 按源码更正 `CSR-14` 证据：普通切换写入点为 `restore_target_to_file_typed`（`:1817`，`:1839` 对父路径 `create_dir_all` 后写入），`:1235-1260` 的 `validate_explicit_destination_path` 只用于显式目标（普通 `run_restore` 传 `None`，`:562`）；GAP-CSR-14 同步；③ 渲染中残留的 `\"` 转义去除（`CSR-23` 编辑器脚本恢复为可直接执行的 `awk` 命令）；④ 收口反复出现的 P2：`CSR-27` 补 `--no-auto-advance --auto-advance` 后者为准（8/8），`CSR-27a` 补 `-U0` 选择性应用（8/8） | 无拆卡 | GAP 表、`CSR-11`、`CSR-14`、`CSR-23`、`CSR-27`、`CSR-27a`、审计表、追溯表、review log |
| 2026-11-18 18:41:45 UTC | 2026-11-18 双审 R11：Claude PASS、Codex PASS（A–E 五分区全部 PASS），受审 SHA-256 `fd3f96ccd5475143abafcee9cb2712e198a7095a3e2b07826d20ce395dd913e4` | 双审 PASS 后的记账修订：只更新头部状态行并追加本行与 review log 的 R11 两行，其余字节与受审字节相同。残余 P2：`CSR-27` 未测反向顺序 `--auto-advance --no-auto-advance`；`CSR-14` 证据把 `run_restore` 传 `None` 记为 `:562`（实际调用跨 `:561-567`）；`DEFER-CSR-02` 延后上游整脚本回归；`CSR-04` Description 仍列 `-B`；切换结果门对 unborn `HEAD` 的「所指提交」表述 | 无 | 头部、修订历史、review log |
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
| 2026-11-17 04:14:00 UTC | Claude & Codex R10 FAIL | `CSR-11` 拆出 `CSR-33`（`6/8` + `3/8`）；拆细 `CSR-04`（`6/8`）与 `CSR-14`（`5/8`）并对齐 `CSR-14` `Granularity` `deps=CSR-29`；`CSR-29` `switch -f` 指定 `<target-branch>`；精确 `cli.rs` 行号（`3029`/`3040`/`3051`/`3062`）并将 `cli.rs:5320` 移至 `CSR-27`；`CSR-17` 补 `switch_test` 验证；同步 `CSR-13` 审计 `3/3` 与追溯表、`#702`/`#1008` 部分编号、Phase 1/2 标题与退出、成功定义 `38` 卡、风险表与完成判据 `CSR-31`/`32` | `CSR-11` → `CSR-11,33` | 全文 |
| 2026-11-17 04:22:00 UTC | Codex R11 FAIL（Claude R11 `ECONNRESET`） | 修正 `CSR-29` 正文 3 条 `switch -f` 判据占位符为 `<target-branch>`（与 `switch.rs:1554` 区分当前分支 no-op）；将 `#712` 重拆为按命令独立行为轴的 `CSR-11`（`checkout` 符号链接安全 + 同卡常规文件/无关修改/索引回归，`6/8`）与 `CSR-33`（`switch` 符号链接安全 + 同卡常规文件/无关修改/索引回归，`6/8`），消除变动与回归测试分离（G-02） | N/A | 审计表、`CSR-11`、`CSR-33`、`CSR-29`、追溯表、实施证据汇总、review log |
| 2026-11-17 04:34:00 UTC | Claude & Codex R12 FAIL | 将 `CSR-33` 合并回 `CSR-11`（`8/8` 原子 AC，`3/8` VER），同卡覆盖 `checkout -f HEAD~`（`switch::checkout_to_commit` `switch.rs:1405,1433`）与 `switch -f <target-branch>`（`move_to_resolved_branch` `switch.rs:1607,1691`），修正 `#712` 实测 `mv dir untracked && ln -s untracked dir` 场景、移除无 Git 依据的普通 checkout 拒绝 AC、明确 `-f` 保留无关未跟踪文件并丢弃已暂存/已跟踪改动使索引与目标树一致；修正 `CSR-11` 根因锚点至 `restore.rs:2206`；在 `CSR-14`/`29`/`30` 补齐 `restore.rs:1941-1997`（`preflight_worktree_restore_to_commit`）及 `switch.rs:1433,1607` 锚点；统一全文至 `32` 卡并修正 `Codex review log` R11 评审者记录 | `CSR-11,33` → `CSR-11` | 全文 |
| 2026-11-17 04:43:00 UTC | Claude & Codex R13 FAIL | 修正 `CSR-12` 残留依赖 `CSR-33` → `CSR-11`；将 `CSR-23` 合回 `CSR-01`（取消 `REL-CSR-01`/`CSR-25`）、将 `CSR-24` 合回 `CSR-09`（`8/8`，取消 `REL-CSR-02`/`CSR-26`）以消除 G-02 实现与互斥校验拆分；按 `plan-template.md` 门族型验收白名单为 `CSR-11`（`10/8@EX-CSR-01`）与 `CSR-01`（`9/8@EX-CSR-02`）登记 waiver 与「判据规范」块；修正 `CSR-11` 不跟随符号链接断言（不强绑必须删除或保留符号链接本身）、`switch.rs:1412`/`1540`、`checkout.rs:1161`、`restore.rs:1941-1997` 精确行号与全文 `28` 卡计数 | `CSR-01,23,25` → `CSR-01`；`CSR-09,24,26` → `CSR-09` | 全文 |
| 2026-11-17 04:56:00 UTC | Codex R14 FAIL（Claude R14 `ECONNRESET`） | 移除 `EX-CSR-01`/`EX-CSR-02` waiver：`CSR-01` 移除冗余 D15 守卫 AC9 收敛至 `8/8`；`CSR-09` 拆出 `CSR-24`（`-p`/`-b`/`-B`/`--orphan` 互斥，`4/8`）使 `CSR-09` 自身含基础读取与核心互斥（`7/8`）；`CSR-11` 拆出 `CSR-33`（`switch -f`，`5/8`）并设家族 `REL-CSR-05`（发布点 `CSR-34`）同版发布双命令校验；登记 `DEP-CSR-07` 串行化跨计划共享 `COMPATIBILITY.md`/`docs/error-codes.md`；统一全文至 `31` 卡 | `CSR-09` → `CSR-09,24`；`CSR-11` → `CSR-11,33,34` | 全文 |
| 2026-11-17 05:09:00 UTC | Codex R15 FAIL | `CSR-09` 纳入全部 7 项 `--pathspec-from-file` 互斥与缺失文件诊断（`8/8`），`CSR-24` 改为独立行为轴 `--pathspec-file-nul`（`3/8`），确保 `CSR-09` 独立发布时不缺任何互斥校验；`CSR-33` 合回 `CSR-11` 并取消 `REL-CSR-05`/`CSR-34`，按 `plan-template.md:572` 为 `CSR-11` 登记 `EX-CSR-01`（`10/8@EX-CSR-01`，10 个独立具名可执行 `nextest` 门禁命令，`Approver: Eli (Plan Owner)`）；`DEP-CSR-07` 明确协调者批次串行调度门控；统一全文至 `29` 卡与 `/Volumes/Data/gitmono/libra-backend` | `CSR-11,33,34` → `CSR-11` | 全文 |
| 2026-11-17 05:12:00 UTC | Codex R16 FAIL（Claude R16 `ECONNRESET`） | 取消 `EX-CSR-01` waiver 并拆卡消除全部复合谓词：`CSR-01` 拆为 `CSR-01` + `CSR-23` 经 `REL-CSR-01` 发布点 `CSR-25` 联合发布；`CSR-09` 拆为 `CSR-09` + `CSR-33`；`CSR-11` 拆为 `CSR-11` + `CSR-35` 经 `REL-CSR-06` 发布点 `CSR-36` 联合发布；`CSR-31`/`CSR-32` 补齐所属家族全部新守卫命令；将 D 组默认显式扩展至 `family release point`（含 artifact/CDN 可用性）；同步 `CSR-05`/`06` `Granularity` `deps=` | `CSR-01`→`01,23,25`；`CSR-09`→`09,33`；`CSR-11`→`11,35,36` | 全文 |
| 2026-11-17 05:17:00 UTC | Codex R17 FAIL | 修正 `CSR-01` 在首卡即解禁 `src/cli.rs` D15 并跑 `compat_declined_interactive_flags`，并将 `y`/`n`/`q` 拆为独立单谓词 AC（`CSR-01` `7/8` + `CSR-23` `8/8`）；重划 `CSR-09`（`--pathspec-from-file=<file>` + 缺失 128 + 全部 6 项互斥，`8/8`）与 `CSR-33`（`--pathspec-from-file=-` stdin，`2/8`，取消多余的 `REL-CSR-05`/`CSR-34`）；将 `CSR-11`/`CSR-35` 由按命令拆分改为按行为轴拆分（`CSR-11` `4/8` 同卡覆盖 `checkout -f` + `switch -f` 符号链接不跟随，`CSR-35` `6/8` 同卡覆盖双命令常规恢复/未跟踪保留/索引同步，经 `REL-CSR-06` `CSR-36` 联合发布）；修正 `M1` 计数为 7 独立 + 3 家族发布点，全量同步 `34` 卡 | 移除 `CSR-34`/`REL-CSR-05` | 全文 |
| 2026-11-17 05:37:00 UTC | Codex R18 FAIL（Claude R18 `ECONNRESET`） | 将 `CSR-35` 回归覆盖并回 `CSR-11` 并移除 `REL-CSR-06`/`CSR-36`（消除 G-02 纯回归测试卡）：`CSR-11`（`8/8`，`independent`）在 `restore.rs:2206` 实施**逐路径组件（component-wise）** `symlink_metadata` 前导符号链接检查（绝不对 `dir/f` 整串检查后穿过前导符号链接删除外部目标）并同卡验证 `checkout -f` 与 `switch -f` 各自退出 0、保护目标目录、写入常规文件及保留未跟踪/索引一致；将当时网站文档路径统一改为本地已挂载的 `../libra-backend`（`DEP-CSR-02` 保留 `../libra-backend` 挂载优先核对规则；当前生产路径已由后续指令改为 `/Volumes/Data/gitmono/libra-backend`）；全量同步 `32` 卡 | 移除 `CSR-35`/`36`/`REL-CSR-06` | 全文 |
| 2026-11-17 05:45:00 UTC | Codex R19 FAIL（Claude R19 `ECONNRESET`） | 移除 `发布分组与并发窗口` 残留 `REL-CSR-06`/`CSR-35`/`CSR-36`，`DEP-CSR-02` 补充 `/Volumes/Data/gitmono/libra-backend` 挂载优先核对，`CSR-11` 将未跟踪保留与索引同步拆为独立谓词（`10/8@EX-CSR-01`） | 移除 `CSR-35`/`36`/`REL-CSR-06` | 依赖表、发布分组、`EX-CSR-01`、`CSR-11`、审计表、review log |
| 2026-11-17 06:58:00 UTC | Codex R20 FAIL（Claude R20 `ECONNRESET`） | 正式恢复登记 `EX-CSR-01`（R21，依 `plan-template.md:572` 门族型验收白名单）；在 `DEP-CSR-02` 明确 `../libra-backend` 与 `/Volumes/Sea/libra-backend` 身份等价/remote/`cf` 分支核对与 STOP 条件（此为历史记录；当前生产路径已由后续指令替换）并接入全部 29 张实现卡；全部 29 张实现卡补齐同卡 ER-06/ER-06a 逐文件文档验收门（不计入 G-03 卡特有 AC 分子）；拆细 `CSR-01`/`05`/`06`/`07`/`12`/`14`/`15`/`16`/`17`/`19`/`20`/`21` 复合 AC 至严格单谓词并全量同步 `Granularity` 与审计表；ER-12 具名发布者为 `Eli` 指定的单一串行发布者 | N/A | 全文 |
| 2026-11-17 07:08:00 UTC | Claude & Codex R21 FAIL | 移除 `EX-CSR-01` 并将 `CSR-11` 收敛至 `8/8`；新建 `REL-CSR-05`（`CSR-09`+`CSR-35`+`CSR-33`+`CSR-24` 经 `CSR-34` 联合发布）消除 `CSR-09` 15/8 超限与 `CSR-33` 独立发布割裂；拆细 `CSR-03`/`18`/`20`/`22`/`24`/`30` 复合 AC 并同步 `Granularity` 与审计表；修正 `CSR-25` `VER=5/12`、`split-from`、`CSR-21` AC5、`compat_declined_interactive_flags.rs` 锚点、`CSR-02` 范围说明、具名发布者 `Eli` 与家族 `cf` 推送时序（全计划 34 卡） | `CSR-09`→`CSR-09,35,34`；移除 `EX-CSR-01` | 全文 |
| 2026-11-17 07:46:33 UTC | Claude & Codex R22 FAIL | 将 `CSR-35` 合回 `CSR-09` 并登记 `EX-CSR-03`；同步 38 张活动卡与 6 个家族发布点；补追溯表、`CSR-11` 双入口回归、家族 `cf` 推送 AC，并拆开 `CSR-12`/`14`/`21`/`33` 复合谓词 | `CSR-35` → `CSR-09` | 评审结论、waiver、审计表、REL-CSR-05、追溯表、里程碑、Review log |
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
| 2026-11-17 17:35:00 UTC | 机械自审：网站路径统一 | 现行网站写集统一 `/Volumes/Data/gitmono/libra-backend`，不改写已推送提交；待定向复审 | N/A | 全文现行网站写集、Review log |
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（当时记为「用户最新指令指定」而无出处；现行依据为用户 2026-11-18 11:55:09 UTC 确认，`AGENTS.md:135` 的 `/Volumes/Sea/libra-backend` 为已知文档滞后且本机不存在），并在 `DEP-CSR-02` 与「文档职责」写明权威依据与 STOP 条件；`CSR-10`/`CSR-10a`/`CSR-15`/`CSR-15a` 回滚改为家族窗口内 `revert`、发布写集改为不得独立发布，与 `CSR-10R`/`CSR-15R` 统一发布约定一致；`CSR-27`/`CSR-27a` 六个选项注册门与五个行为门改用完整限定名精确过滤并要求先用 `cargo nextest list` 确认完整限定名恰好命中一个，再运行并记录退出码、匹配数、通过数、失败数与跳过数；缺项、零执行、失败或跳过即 STOP；15 个 `--pathspec-from-file` 门同样改为 `test(=command::checkout_test::<完整名>)`；`CSR-09`/`CSR-24`/`CSR-34` 改用 `command::` 前缀精确过滤；追溯表补 `CSR-10R`/`CSR-15R` 两行并把 `cli.rs` 与守卫测试从 `WC-11a` 移至 `WC-11b`；R78-A 标为拆分前历史批准；风险登记补 `CSR-10`/`10a`/`15`/`15a`/`27a`；Phase 1/2 与 M1/M2 补齐拆分卡 | N/A | 全文对应章节 |
| 2026-11-18 09:43:54 UTC | Codex 复审 FAIL 修复（验证契约与门禁；本行原记 `2026-11-18 10:35:00 UTC`，晚于实际写入时刻：文件 mtime 表明写入不晚于 `2026-11-18 09:43:54 UTC`，确切时刻不可证实，2026-11-18 双审前按 mtime 更正） | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 17:59:49 UTC | 2026-11-18 双审 R1 前专项清扫（Claude，受审前；起始 SHA-256 `3849096a061fe40fc76ee436855323b231980d806eeb9718799aad0bc23875ee`） | ① 计划文本提交门改为 Claude（Subagent）与 Codex 双审（用户 2026-11-18 指令；执行期 ER-05 与网站 Claude Code PASS 门不变）；② 网站路径依据改为「用户 2026-11-18 11:55:09 UTC 确认」，删去无出处的「用户最新指令」，`AGENTS.md:135` 旧路径登记为已知文档滞后；③ 补写此前被 32 处引用却不存在的「文档与网站验收命令」（第 1–8 步、W-1/W-2，进程内比对、不落盘证据），重写「验证执行协议」（精确过滤与预检、Git 期望、稳定错误码、仓库快照 `S` 与副作用边界、故障注入、非测试型 V、夹具有效性对照）；④ 撤除上一行机械修订生成的无效「AC → 断言映射」（发布点 AC 映射到 cargo 测试、断言 stdout 含源码行号、`libra log` 套用 nextest 协议、`--test compat_declined_interactive_flags` 未精确过滤等），38 张活动卡逐卡重写 AC、带夹具的断言映射与 Verification；失败路径统一断言稳定错误码（新增 ADR-CSR-03），`CSR-09` 由 15/8 收敛为 8/8 并撤销 `EX-CSR-03`；⑤ `CSR-27` 改为 `--[no-]auto-advance` 轴（仍写 `src/cli.rs`），`CSR-27a` 改为 `-U`/`--inter-hunk-context` 轴（实现与测试同卡，不再是纯验证卡），#702 部分编号改为 1/4..4/4；⑥ 跨计划：新增 `DEP-CSR-08`（对等 `DEP-PM-15`）、`DEP-CSR-09`（同批写集协调窗口，含 `DEP-GII-03`、commit-signing 的 merge 卡与网站生成产物）、`DEP-CSR-10`（`DEP-CS-05` 映射）、`DEP-CSR-11` 与 `DEFER-CSR-03`（`DEP-FP-03` 移交未承接）、`DEP-CSR-12`（复用 `DIFF-09`），重写 `DEP-CSR-02`/`03`/`04`/`06`；⑦ 家族子卡 `CSR-10`/`10a` 的 `Full-suite trigger` 由误写的 `T-1` 改为 `N/A`，独立卡统一写明 `install.sh`/`install.ps1` bump 触发的 `T-1`；含网站页的卡写集补网站生成产物；发布点不再把 ER-04 全量登记为 V；⑧ 测试矩阵、追溯表、里程碑 M0、风险、延后项与完成判据同步 | `CSR-27`/`CSR-27a` 轴重划（编号不变）；撤销 `EX-CSR-03` | 全文 |

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

### ADR-CSR-03: 参数冲突与失败路径沿用 Libra 稳定错误码

- **Status:** Accepted
- **Context:** Git 对参数冲突与多数致命错误以 `die()` 退出 128；Libra 的 `CliError` 由稳定码派生退出码（例如 `LBR-CLI-002` 派生 129，`src/utils/error.rs:1066-1076`、`docs/error-codes.md:97-121`），唯一登记的 128 例外是 `update-ref`（`docs/error-codes.md:173-179`）。上游测试的 `test_must_fail` 同时接受 128 与 129。
- **Decision:** 本计划失败类 AC 以稳定错误码判定（Issue 验收要求的诊断文案另列，见各卡；目前为 #702 的 `-p --overlay`、#708 的 `--detach`/`--patch`/位置 pathspec/`--pathspec-file-nul` 与 #721 的 `matched multiple (2)`）：参数冲突 `LBR-CLI-002`、无效修订 `LBR-CLI-003`、覆盖保护与 DWIM 歧义 `LBR-CONFLICT-002`（沿用 `switch.rs:264-268` 现有映射）、读失败 `LBR-IO-001`、写失败 `LBR-IO-002`；数值退出码取默认派生值，不为 `checkout`/`switch`/`restore` 新增 128 例外；Issue 明确要求的 Git 诊断文案单列为独立 AC。
- **Alternatives considered:** 逐处 `with_exit_code(128)` 复刻 Git。拒绝理由：扩大 `docs/error-codes.md` 的例外面并改变已发布的退出码契约。
- **Consequences:** 原「退出 128」与「stderr 含互斥错误」成对 AC 合并为「以稳定码拒绝」，`CSR-09` 由 15/8 收敛为 8/8；`docs/error-codes.md` 不新增条目。
- **Revisit when:** 用户要求 `checkout` 系命令采用 Git 数值退出码时。

## 全局工程约束

继承 `plan-template.md` 全部 `GC-01`..`GC-13`。补充本计划专项约束：
- **GC-CSR-01:** 凡修改用户可见命令行为的卡，必须在同一卡更新对应命令 `docs/commands/<cmd>.md`（EN+zh-CN）、`docs/development/commands/<cmd>.md`、网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md` 及 `COMPATIBILITY.md`，并按「执行检查必备需求」中的「文档与网站验收命令」逐步验收（同步与漂移检查、前端 typecheck/build、绑定最终网站字节的独立 Claude Code PASS、`cf` 签名+DCO 提交推送与部署核验）；任一失败保持未完成。家族子卡只做第 1–4 步，第 5–7 步由唯一发布点执行（第 8 步）。
- **GC-CSR-02:** 涉及工作区文件/目录/符号链接删除或覆盖时，必须使用 `std::fs::symlink_metadata` 判别类型，严禁跟随符号链接删除或写入目标目录（GC-07）。

## 执行检查必备需求（强制）

继承 `plan-template.md` 全部 `ER-01`..`ER-14`。
- 每张卡开工前执行 `ER-01`、`ER-02`（刷新行号锚点）与 `ER-03`（复核 `G-*`）。
- 独立卡（`Release boundary: independent`）完成后执行 `ER-04`、`ER-05`、`ER-07`、`ER-08`（`patch + 1`、三处版本面同步、工具链刷新 `Cargo.lock`、`gh release create`）与 `ER-13`/`ER-14`，并完成 C/D 组。
- 家族子卡（`family child`）只完成本地 A/B、`ER-05` 与该卡写明的 `fmt`/`clippy`/focused 测试。子卡不 bump、不推送、不执行 `gh release create`，也不单独关闭 C/D。其 C/D 由该卡 `C/D coverage from` 指向的家族发布点承载（G-08）。
- 家族发布点（`family release point`）在所属子卡全部本地完成之后，才执行 `ER-08` 与 C/D 组。
- 全部 38 张活动卡完成后执行计划收口全量门：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

- **验证执行协议（对全计划每条 `Verification` 条目强制）：**
  1. **卡特有用例**（标「卡特有」，新增者另标 `(new)`）：每条 V 只含一个测试，过滤式为 `-E 'test(=<完整路径>)'` 的精确匹配；`command_test` 内完整路径为 `command::<模块>::<函数>`（`tests/command_test.rs:10` 以 `mod command;` 挂载 `tests/command/`，模块声明见 `tests/command/mod.rs:1020`（`checkout_test`）、`:1097`（`merge_test`）、`:1124`（`restore_test`）、`:1145`（`status_test`）、`:1150`（`switch_test`）），lib 单测为 `cli::tests::<函数>`（`src/cli.rs:4256` 的 `mod tests`）；不得以子串、前缀、正则或 `|` 并集代替。
  2. **预检：** 运行前以同一前缀与过滤执行 `cargo nextest list`（例：`source .env.test && cargo nextest list --test command_test -E 'test(=command::checkout_test::checkout_patch_mode)'`），列出的测试必须恰为 1 个且名字逐字一致，否则 STOP。预检与第 3 项运行判定合为该 V 的同一个通过/失败门（G-03 按 1 门计）。
  3. **运行判定：** 记录 exit code 与「匹配 / 运行 / 通过 / 失败 / 跳过」计数，要求 exit 0、匹配 = 运行 = 通过 = 1、失败 = 0，否则 STOP；nextest 汇总的「跳过」包含被过滤式排除的测试，只记录、不作判据；目标测试自身不得以提前返回代替断言。
  4. **断言口径：** 测试逐条实现卡内「AC → 断言映射」，执行记录逐条引用 AC 编号；映射与 AC 表述同一谓词，只补充判定值与夹具。
     - (a)「Git 期望」= 开工时在同内容夹具上，以 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（v2.56.0；本机检出 `/Volumes/Data/gitmono/git`，以 `git -C /Volumes/Data/gitmono/git archive 6de20f6092 | tar -x -C <构建目录>` 取得源码后构建，只读、不改动该检出）运行映射所列 `git` 命令（同一 stdin 序列与编辑器脚本）得到的退出码与输出，硬编码进测试；Git 运行环境为 `GIT_CONFIG_NOSYSTEM=1`、`HOME`/`XDG_CONFIG_HOME` 指向测试临时目录、`LC_ALL=C`、`TZ=UTC`，作者/提交者与日期经 `GIT_AUTHOR_*`/`GIT_COMMITTER_*` 固定；映射中的 `git config <键> <值>` 在 Libra 侧以 `libra config set <键> <值>` 写入测试仓库本地配置（Libra 无全局 `-c`）。提交对象名按同夹具对应关系替换为 Libra 侧取值（`libra rev-parse <rev>`）后比较；blob/tree 对象名不替换（同内容即相同）。Git 提示文字中的 `git <子命令>` 按 Libra 既有口径替换为 `libra <子命令>`（例如 `src/command/status/output.rs:735,751` 的 `libra pull`）。映射括注的具体值是评审时按 Git 语义推得的线索，与 pinned Git 实测不一致时以实测为准并回写本卡 `Current evidence`。
     - (b) 成功类映射以 exit 0 为读取输出与状态的前提；「等于 Git 期望」指逐字节相等；「`S.wt` 等于 Git 期望」指按第 6 项规则对 Git 夹具工作区取得的清单逐项相等；「结果状态等于 Git 期望」是单一谓词：被测调用后映射点名的 `S` 分量作为一个整体与 Git 夹具的对应结果比较。
     - (c) 失败类 AC 一律写成「以 `<LBR-*>` 拒绝」或「以 `<LBR-*>` 失败」：映射断言 exit 非 0、stderr 含 `Error-Code: <LBR-*>` 行，并断言第 6 项仓库快照 `S` 不变；映射写明「冲突结束」的调用（如合并冲突）只断言稳定码与映射所列状态，不适用 `S` 不变；`base_libra_command` 以 `env_clear()` 启动子进程（`tests/command/mod.rs:354-372`），`LIBRA_FINE_EXIT_CODES` 与 `LIBRA_ERROR_JSON` 不生效；AC 括注的数值退出码是该稳定码在默认配置下的派生值（`docs/error-codes.md:97-121`；ADR-CSR-03），不作独立判据。Git 诊断文案只在 Issue 验收明确要求时按 ADR-CSR-03 单列为独立 AC（断言 stderr 含该子串）。
     - (d) 交互类（`-p`）用例经 `run_libra_command_with_stdin`（`tests/command/mod.rs:417`）喂入映射所列答案序列；`e` 编辑用例经 `run_libra_command_with_stdin_and_env`（`:439`）把 `GIT_EDITOR` 设为映射所列脚本；Git 侧使用同一序列与脚本。
     - (e) 夹具复用：除映射写明「同一次调用」「同一次运行」或「AC<n> 那次调用」外，每条 AC 在按本卡夹具全新构造的仓库上执行，互不继承状态。
  5. **共享回归组**（标「共享回归组」）：既有用例、`test(/^…::/)` 模块正则族或整 target；运行前以同一过滤 `cargo nextest list` 记录匹配数 N（必须 ≥ 1），运行后要求 exit 0、运行 = 通过 = N、失败 = 0（「跳过」含过滤排除的测试，只记录）。只作 ER-04 A 组回归证据，不计为卡特有 AC 证据，也不计入 `VER` 分子。
  6. **仓库快照 `S` 与副作用边界：** `S` 在被测调用前后各取一次，由下列分量组成：`S.wt` = 工作区清单（以 `symlink_metadata` 递归遍历、不跟随符号链接；每项为相对路径 → 类型（文件/目录/符号链接）、可执行位、文件内容 SHA-256 或链接目标字符串；排除 `.libra/` 与测试 harness 在 `current_dir` 下建立的 `.libra-test-home/`，见 `tests/command/mod.rs:354-355`；Git 夹具侧同样排除其仓库元数据 `.git`（目录或文件），两侧只比较用户工作区）；`S.idx` = 以 `git_internal::internal::index::Index::load` 读取 `.libra/index`（与 `src/command/checkout.rs:695-697` 同一读取入口）后，按（路径, stage）排序的每个条目的 mode、对象名、stage、路径与标志位 `assume_valid`/`skip_worktree`/`intent_to_add`（git-internal 0.10.4 `IndexEntry.flags` 的 `Flags` 字段），不比较 stat 缓存字段；映射中的 `libra ls-files --stage` 只用于对象名或 stage 的局部断言；`S.ref`、`S.log`、`S.cfg` = `.libra/libra.db` 中 `reference`、`reflog`、`config_kv` 与旧 `config` 表的全部行（按主键排序；表定义 `sql/sqlite_20260309_init.sql:5,12,19,31`）；`S.obj` = `libra cat-file --batch-check --batch-all-objects` 中类型为 `commit`/`tag` 的行集合；`S.ext` = 映射点名的仓库外或未跟踪目录按 `S.wt` 规则取得的清单。「`S` 不变」= 各分量逐字相等；失败路径允许新增未被引用的 blob/tree，但 `S.obj` 不得增减行。**副作用边界（全局门，不计入各卡 AC）：** 失败路径按第 4(c) 项要求 `S` 整体不变。成功路径的被测调用前后，`S.cfg` 与 `S.obj`（commit/tag 行）必须逐字不变，除非映射声明其变化（如 `CSR-10` 的上游配置、`CSR-15`/`CSR-22` 的首次提交）；`S.wt`、`S.idx`、`S.ref`、`S.log` 的预期变化以映射所列断言为准（成功的分支切换按 Git 语义更新 `HEAD` 与 reflog，不另作判据），映射写明「不变」的分量须逐字不变；成功调用按 Git 语义新建分支并设置上游时，`branch.<新分支>.remote`/`.merge` 两行允许出现且须等于 Git 期望，`S.cfg` 其余行不变。**切换结果门（全局门，不计入各卡 AC）：** 卡特有用例中每个成功（exit 0）的分支切换或创建类被测调用（`checkout`/`switch` 切到分支或提交，含 `-b`/`-B`/`-C`/`--orphan`/`--detach`/`--track`/`-m` 与 DWIM 新建分支）之后，`HEAD` 的指向（分支名或分离对象名）及其所指提交、`S.wt` 与 `S.idx` 必须等于按同一步骤构造的 Git 夹具在同一调用后的结果（分支名与对象名按第 4(a) 项对应，`S.wt` 两侧按本项规则排除仓库元数据），任一不等即该用例失败；映射对同一调用另有更具体断言的，两者都须成立。**路径恢复的副作用边界：** 以索引为源、只更新工作区的路径恢复（`restore` 不带 `--staged`、`checkout` 不带 tree-ish 的 `-- <path>`/`-p`/`--ours`/`--theirs`/`--pathspec-from-file`）调用前后 `S.idx` 必须逐字不变；`restore --staged`（不带 `--worktree`）调用前后 `S.wt` 必须逐字不变；映射另行断言该分量的除外。**引用断言口径：** 映射中的引用断言按 Libra 存储判定（表定义 `sql/sqlite_20260309_init.sql:19-30`，写入逻辑 `src/internal/head.rs:589-625`）：本地分支 `refs/heads/<b>` 存在 ⇔ `reference` 表有 `kind = 'Branch'`、`remote IS NULL`、`name = '<b>'` 的行；远程追踪 `refs/remotes/<r>/<b>` ⇔ `kind = 'Branch'`、`remote = '<r>'`、`name = '<b>'`；「`HEAD` 指向 `refs/heads/<b>`」⇔ 本工作区 `kind = 'Head'` 行 `name = '<b>'` 且 `commit IS NULL`；分离 `HEAD` ⇔ 该行 `name IS NULL` 且 `commit` 为对应对象名；「无 `refs/heads/<b>`」即无上述 Branch 行；`libra rev-parse refs/heads/<b>` 只在该行存在时使用。
  7. **进程级隔离：** 新增用例一律以子进程调用 `libra`/`git`，以显式 `current_dir` 与逐子进程环境变量传入设置，不修改测试进程 cwd 与环境，因而不使用 `#[serial(...)]`，不触及 `tests/SERIAL_REGISTRY.tsv` 与 `.config/nextest.toml`；若实现中确需 `#[serial]`，先按 ER-03 把这两个文件加入本卡写集并补 `compat_serial_registry` 共享回归门。调用外部 `git` 的用例在 `git` 不在 `PATH` 时必须失败而非跳过。夹具中的「远端仓库」一律是测试临时目录内的本地 Libra 仓库，不访问网络。
  8. **故障注入：** 映射写有 `LIBRA_TEST_CHECKOUT_<名>=1` 的用例，仿 `src/command/tag.rs:889-900` 的现有模式新增仅在 `LIBRA_TEST=1` 且该变量为 `1` 时生效的注入点（release 构建不生效），由子进程环境传入；注入点位置与名称写进本卡 `Current evidence`。
  9. **非测试型 V：** 祖先校验、release note、文档哈希核对、零命中守卫与网站步骤等不运行 nextest 的 V 不适用第 1–3 项；按卡内写明的命令与退出码判定，记录命令、退出码与结论，任一不满足即该门 FAIL；计入 `VER` 分子。
  10. **夹具有效性对照：** 映射写有「对照：…」时，开工取 Git 期望须核对该 AC 的 Git 期望与对照条件下的 Git 结果确实不同；相同即夹具无区分力，须调整夹具后重取期望，并把最终夹具与两份结果的 SHA-256 写入本卡 `Current evidence`。这是夹具有效性门，不计入 AC 或 VER。

- **文档与网站验收命令（ER-06/ER-06a 同卡强制门；各卡「文档 AC」引用；按序执行，任一步失败即 STOP 且本卡不得进入 `complete`）：**
  1. **仓库文档逐文件验收：** 前置：持有 `DEP-CSR-07` 对 `COMPATIBILITY.md`/`docs/error-codes.md` 的窗口及适用的 `DEP-CSR-08`/`DEP-CSR-09` 窗口（卡内记录窗口起止 UTC）。对本卡 `Docs and compatibility impact` 所列每个仓库文件记录改动前后 `shasum -a 256 <file>`；以 `test -f docs/commands/<cmd>.md && test -f docs/commands/zh-CN/<cmd>.md` 确认 EN/zh 双份存在；对卡内「文档关键字」在每个受影响命令的 EN/zh 命令页执行 `rg -n -F -- '<关键字>' <file>`（退出码 0 = 命中；1 = 缺失 → STOP；>1 = 命令失败 → STOP；关键字分 EN/zh 写出时各查各的页），且这些命令页改动后 SHA-256 必须与改动前不同（本计划每张实现卡都改变用户可见行为，命令页必须同卡实际更新）；`docs/development/commands/<cmd>.md`、`docs/development/commands/_compatibility.md`、`tests/compat/README.md`、`docs/error-codes.md` 与 `COMPATIBILITY.md` 对应行逐文件做语义核对，结论为「已更新」（记录改动前后 SHA-256 与所改段落）或「无需修改」（记录理由与未变 SHA-256）；`COMPATIBILITY.md` 先以下方命令块 W-1 刷新目标行号（必须恰好输出一个行号；2026-11-18 基线 `restore`=190、`status`=191、`switch`=210、`merge`=212、`checkout`=273；`merge` 另有 `:205` 的 `supported` 附录行，W-1 对 `merge` 用 `partial` 主行变体），有改动时断言改动前后 `wc -l < COMPATIBILITY.md` 相等（原地修改）。
  2. **网站检出预检：** 前置：`DEP-CSR-02`（用户 2026-11-18 11:55:09 UTC 确认）未被用户撤回或改变，且持有 `DEP-CSR-09` 对网站交付（页面与生成产物）的串行窗口及适用的 `DEP-CSR-08`，否则 STOP。按序执行：(a) `test -d /Volumes/Data/gitmono/libra-backend/.libra && test ! -e /Volumes/Data/gitmono/libra-backend/.git`，不满足即 STOP；(b) `cd /Volumes/Data/gitmono/libra-backend && libra --offline symbolic-ref --short HEAD`，输出不是 `cf` 时按 ER-06a 执行 `libra switch cf`（被脏工作区等阻止即 STOP）后重跑 (b)；(c) 此后 `libra --offline symbolic-ref --short HEAD` 输出须恰为 `cf`，且 `libra status --short --branch` 首行须以 `## cf...origin/cf` 开头，否则 STOP；(d) 本卡目标页面 `apps/tanstack-app/content/docs/commands/<cmd>.en.md` 若已有未提交改动（`libra status --short -- <页面路径>` 输出非空）即 STOP（家族例外见第 8 步）；随后执行 `libra fetch origin`，要求 `libra rev-parse HEAD` 等于 `libra rev-parse refs/remotes/origin/cf`——本地落后时只允许 `libra pull --ff-only origin cf` 快进后重验，领先或分叉即 STOP；`libra status --porcelain` 中不得有任何已暂存条目（首列既非空格也非 `?`），否则 STOP。已有的无关改动保留、不暂存。
  3. **同步与漂移检查：** 以包含本卡最终 EN 参考文档的 checkout 为 `--libra-root`（`/Volumes/Data/gitmono/libra`；执行前以第 1 步记录的 SHA-256 确认其中 `docs/commands/<cmd>.md` 已是本卡合入 `main` 的字节；家族子卡改为确认它等于本卡本地签名提交中的字节，即与 `libra show <本卡提交>:docs/commands/<cmd>.md | shasum -a 256` 相同，见第 8 步），执行 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --write --only <cmd…>`（多命令以空格分隔），随后 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --only <cmd…>` 必须 exit 0（1 = 漂移、2 = 安全拒绝，均 STOP；脚本参数见 `scripts/sync-libra-command-docs.py:406-422`）；记录每个 `apps/tanstack-app/content/docs/commands/<cmd>.en.md` 的改动前后 SHA-256，同步后的页面 SHA-256 必须与 `cf` 已提交版本（`libra show HEAD:<页面路径> | shasum -a 256`）不同，相同即 STOP 排查。
  4. **前端校验：** `cd /Volumes/Data/gitmono/libra-backend && pnpm typecheck` 与 `pnpm build` 分别记录退出码，均须为 0。`pnpm typecheck` 经 `gen:docs`（`apps/tanstack-app/package.json:12-13`）重写被忽略的生成文件 `apps/tanstack-app/src/generated/docs-manifest.ts` 与 `docs-search.ts`，`pnpm build` 重写被忽略的 `apps/tanstack-app/dist/`；这三处是只由生成器/构建写入的派生产物，已列入每张含网站页面的卡的写集，并按 `DEP-CSR-09` 只在持有网站交付串行窗口时重生成；执行前后只记录两个生成文件的 SHA-256（不存在记 `absent`）与 `dist/` 是否存在及其文件数（`find apps/tanstack-app/dist -type f | wc -l`），这些记录不作判据；不得手工编辑、暂存或提交这三处。
  5. **网站评审：** 对第 3 步记录的最终页面字节取得独立 Claude Code `VERDICT: PASS`；评审后页面字节变化须重审。
  6. **精确提交与推送：** 只 `libra add apps/tanstack-app/content/docs/commands/<cmd>.en.md`（多命令卡逐页添加）→ `libra status --porcelain` 的已暂存条目必须恰为本卡页面集合，否则 STOP → `libra commit -s -m "docs(commands): sync <cmd> for <卡 ID>"` → 按 ER-07 对同一提交校验 `gpgsig` 与 `Signed-off-by` → `libra show <提交>:<页面路径> | shasum -a 256` 等于第 5 步受审字节 → 推送前再次 `libra fetch origin`，要求 `libra rev-parse <提交>^` 等于 `libra rev-parse refs/remotes/origin/cf` → `libra push origin cf`，推送后 `libra ls-remote origin refs/heads/cf` 指向该提交。远端已前进（比较不等或推送因非快进被拒）时不推送、不 force、不改写他人提交，按以下恢复后从第 2 步重新开始：`libra reset --mixed <提交>^`（只撤销本卡这一个未推送提交，工作区与他人改动保留）→ `libra restore --staged --worktree -- <页面路径>` → `libra pull --ff-only origin cf`；重新生成的页面字节与第 5 步受审字节不同时须重新取得第 5 步 `PASS`。
  7. **部署核验（`<article>` HTML 逐字节比对）：** 基准构建前先过干净性门：`cd /Volumes/Data/gitmono/libra-backend && libra status --short --ignored --untracked-files=all` 的输出中，除后端根目录 `docs/` 下的条目与下列构建产物/依赖/缓存的忽略条目外不得有任何条目：`node_modules/`、`apps/tanstack-app/node_modules/`、`apps/tanstack-app/dist/`、`apps/tanstack-app/.wrangler/`、`apps/tanstack-app/src/generated/docs-manifest.ts`、`apps/tanstack-app/src/generated/docs-search.ts`、`scripts/__pycache__/`、`test-results/`、`apps/tanstack-app/.dev.vars`；后端根目录不得存在任何 `.env*` 文件；并要求 `libra rev-parse HEAD` 等于 `libra rev-parse refs/remotes/origin/cf`。任一不满足即 STOP（不清理、不暂存他人改动）。随后以 `pnpm preview:cf`（`apps/tanstack-app/package.json:9`，先构建再以 `wrangler dev --port 7001` 本地运行）作基准，对本地 `http://localhost:7001/en/docs/commands/<cmd>` 与生产 `https://libra.tools/en/docs/commands/<cmd>` 分别执行下方命令块 W-2：它在内存中取得同一次响应，要求 HTTP 状态 200、`<article>` 存在且文本非空，输出 `<article>` 内部 HTML 原始字节的 SHA-256 与字节数；退出码非 0（1 = 请求或 HTTP 错误、2 = 缺 `<article>` 或文本为空、3 = 状态非 200）即 STOP。两份 SHA-256 必须相等，不相等即 STOP。只把两份哈希、字节数与观察 UTC 时间写入本计划，不落盘任何响应内容；用后停止本地预览进程。部署传播延迟按 ER-10 暂时性错误有界重试（≤ 5 次、≤ 30 分钟），超预算置 `blocked`。
  8. **时序与家族：** 独立卡第 1–2 步在本卡 C 组第⑦步签名提交之前完成，任一失败则本卡不得提交、合并或发布（`Lifecycle` 置 `blocked`）；第 3–7 步在本卡 Libra PR squash merge（C 组第⑧步）与 `gh release create`（第⑨步）之后执行，使网站不先于发布公开新行为（第 3–7 步开始前重跑第 2 步），未完成前本卡停留在 `remote-pending`。家族子卡（`REL-CSR-01`：`CSR-01`/`CSR-23`；`REL-CSR-03`：`CSR-08`/`CSR-28`；`REL-CSR-04`：`CSR-02`/`CSR-27`/`CSR-27a`/`CSR-03`；`REL-CSR-05`：`CSR-09`/`CSR-33`/`CSR-24`；`REL-CSR-TRACK`：`CSR-10`/`CSR-10a`；`REL-CSR-ORPHAN`：`CSR-15`/`CSR-15a`）在本地签名提交前执行第 1–2 步、提交后执行第 3–4 步（`--libra-root` 为含本卡本地提交的 checkout；网站页留作 `libra-backend` 中的本地未提交改动并登记 SHA-256），不执行第 5–7 步；同一家族内后一张子卡第 2(d) 步只允许该页面存在家族内此前最后修改它的子卡登记过 SHA-256 的未提交改动，其第 3 步同步后的页面须无漂移（检查模式 exit 0）。网站页的第 5–7 步由唯一发布点在其 squash merge 与 `gh release create` 之后，以家族最终 EN 文档重跑第 2–4 步（第 2(d) 步对家族交付的每个页面，只允许存在家族内最后修改该页面的子卡登记过 SHA-256 的未提交改动；例如 `REL-CSR-03` 的 `status.en.md` 由 `CSR-08` 最后修改、`checkout.en.md` 由 `CSR-28` 最后修改）后执行一次；家族窗口期内其它卡（含其它计划的 `PM-21a`、`GII-21`、`GII-23`）不得对同一页面进行网站交付（`DEP-CSR-08`）。

上列第 1、7 步引用的命令块（`<cmd>` 替换为本卡命令；`<URL>` 为第 7 步的本地或生产页面地址）：

```bash
# W-1：定位 COMPATIBILITY.md 中 <cmd> 的状态矩阵行；必须恰好输出一个行号
awk -F'|' -v c=" <cmd> " '$2==c && $3 ~ /^ (supported|partial|unsupported|intentionally-different) $/ {print NR}' COMPATIBILITY.md
# W-1（merge 变体）：只取 partial 主行，必须恰好输出一个行号
awk -F'|' '$2==" merge " && $3==" partial " {print NR}' COMPATIBILITY.md

# W-2：在内存中取得 <URL> 的同一次响应，输出 <article> 内部 HTML 原始字节的「SHA-256 字节数」；退出码须为 0，不落盘响应
python3 -I - "<URL>" <<'PY'
import hashlib, html, re, sys, urllib.request
req = urllib.request.Request(sys.argv[1], headers={"User-Agent": "curl/8.7.1"})
with urllib.request.urlopen(req, timeout=30) as r:
    if r.status != 200:
        sys.exit(3)
    s = r.read().decode("utf-8")
m = re.search(r"<article>(.*?)</article>", s, re.S)
if not m:
    sys.exit(2)
a = m.group(1)
if not " ".join(html.unescape(re.sub(r"<[^>]+>", " ", a)).split()):
    sys.exit(2)
b = a.encode("utf-8")
print(hashlib.sha256(b).hexdigest(), len(b))
PY
```

## 实施顺序

按 G-10 串行化写集，形成与 Phase 0/1/2 一致的无环顺序链：
- `CSR-01 -> CSR-23 -> CSR-25 -> CSR-02 -> CSR-27 -> CSR-27a -> CSR-03 -> CSR-32 -> CSR-04 -> CSR-05 -> CSR-06 -> CSR-07 -> CSR-08 -> CSR-28 -> CSR-31 -> CSR-09 -> CSR-33 -> CSR-24 -> CSR-34 -> CSR-10 -> CSR-10a -> CSR-10R -> CSR-11 -> CSR-12 -> CSR-13 -> CSR-29 -> CSR-14 -> CSR-15 -> CSR-15a -> CSR-15R -> CSR-16 -> CSR-17 -> CSR-18 -> CSR-19 -> CSR-20 -> CSR-30 -> CSR-21 -> CSR-22`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-CSR-01 | incoming | 上游基线 | `main@62abb622f0eb2626934633ada78061963ebf1f44` | 仓库维护者 | 干净可编译基线且 `compat_version_surface_sync` 通过 | `Cargo.toml:3` | 若版本面不一致，先建修复卡对齐再开工 |
| DEP-CSR-02 | incoming | 相邻网站仓库（用户确认） | `/Volumes/Data/gitmono/libra-backend` 的 `cf` 工作区（`.libra` 仓库、无 `.git`）：用户 2026-11-18 11:55:09 UTC 确认为本批网站交付检出；仓库 `AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`（本机不存在），属已知文档滞后，本计划不修改该文件 | 文档发布者（ER-12 发布者） | 「文档与网站验收命令」第 2 步预检全部通过 | 第 2 步各命令输出与退出码记入卡内 | 预检任一项不满足即 STOP；用户撤回或改变该确认时 STOP，不强行写入 |
| DEP-CSR-03 | incoming | 跨计划共享 `src/cli.rs` 串行窗口 | 同批写 `src/cli.rs` 的在途卡（按 ID 引用、以对方提交版本为准）：`plan-20261117-commit-signing.md#CS-08` 及其后写者（对方 `DEP-CS-05` 已把它们门控在本计划之后，见 `DEP-CSR-10`）；`plan-20261117-worktree-clean-reset-apply.md` 的 `WC-11` 家族（对方 `DEP-WC-07`）；`plan-20261117-global-init-infra.md#GII-01..GII-05,GII-15,GII-24`（对方 `DEP-GII-03`）；`branch`/`clone`/`fetch-push`/`plumbing-misc` 计划中写 `src/cli.rs` 的卡 | 仓库协调者 | 本计划写 `src/cli.rs` 的卡（`CSR-01`、`CSR-23`、`CSR-02`、`CSR-27`）在所属家族首张写 `src/cli.rs` 的子卡开工前取得家族级单写者窗口，持有至家族发布点（`CSR-25`、`CSR-32`）squash merge 合入 `main` 后释放 | 协调者放行记录（起止 UTC）记入首张子卡与发布点 | 未获窗口前该卡保持 `pending`，不编辑 `src/cli.rs`（G-10）；超过 7 日报告协调者 |
| DEP-CSR-04 | incoming | 同批并行计划 | `plan-20261117-merge-pull.md` 的 `MP-17`..`MP-25`（共享 `src/command/merge.rs`/`merge/rename_merge.rs`；对方计划尚未提交至 `main`，按 ID 引用、以其提交版本为准） | 协调者 | 对方上述卡已合入 `main` 后 `CSR-21` 基于最新 `main` 开工 | `plan-status.md` 与对方计划 | 未满足前 `CSR-21` 保持 `pending`；对方卡号或范围变化时按其提交版本重新核对并在本表注明 |
| DEP-CSR-05 | incoming | 同批并行计划 | `plan-20261117-worktree-clean-reset-apply.md`（共享 `src/command/restore.rs`、`tests/command/restore_test.rs` 与 restore 文档；对方 `DEP-WC-04` 对等登记本计划 14 张写 `restore.rs` 的卡，与本计划写集一致） | 协调者 | 双方对 `restore.rs` 的改动按串行合并窗口错开（自 `CSR-01` 起生效；家族卡持有至发布点合入 `main`） | `plan-20261117-worktree-clean-reset-apply.md#DEP-WC-04` | 禁止并发修改 `restore.rs`（G-10） |
| DEP-CSR-06 | incoming | 同批并行计划 | `plan-20261117-branch.md`（共享 `src/command/branch.rs`；对方计划尚未提交至 `main`，按 ID 引用、以其提交版本为准） | 协调者 | `CSR-10` 开工前取得 `branch.rs` 单写者窗口，持有至 `CSR-10R` 合入 `main` | `plan-status.md` | 禁止与对方写 `branch.rs` 的卡并发修改（G-10） |
| DEP-CSR-07 | incoming | 跨计划共享文档写集 | `COMPATIBILITY.md`, `docs/error-codes.md`（与并行计划 `branch`/`grep`/`diff` 共享） | 仓库协调者 | 由协调者在 `plan-status.md` 对同批全部并行计划统一执行串行调度门控：独立卡每卡编辑共享文件前须取得协调者书面单写者放行，合入后立即释放；家族卡（`REL-CSR-01`/`03`/`04`/`05`/`REL-CSR-TRACK`/`REL-CSR-ORPHAN`）在首张子卡开工前取得**家族级锁**并沿同家族子卡内部顺序传递，直至家族发布点合入 `main` 后统一释放（窗口期禁止其它计划插入）；`COMPATIBILITY.md` 仅原地改 `checkout`/`switch`/`restore`/`status`/`merge` 行，`docs/error-codes.md` 允许相关卡复用/追加错码行 | 协调者批次串行调度表与逐卡/逐家族放行记录（合入后同步 `plan-status.md`） | 未获协调者当卡/当家族串行放行前阻塞开工编辑对应共享文件，禁止与 `branch`/`grep`/`diff` 并发修改同一文件（G-10） |
| DEP-CSR-08 | incoming | 跨计划共享文件写锁（checkout；与 `DEP-PM-15` 对等） | `src/command/checkout.rs`、`tests/command/checkout_test.rs`、`docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md` 与网站 `apps/tanstack-app/content/docs/commands/checkout.en.md`：同批 `plan-20261117-plumbing-misc.md#PM-21a`（对方 `DEP-PM-15`）与 `plan-20261117-global-init-infra.md#GII-21,GII-23`（对方 `DEP-GII-03`）亦写（按 ID 引用、以对方提交版本为准） | 仓库协调者 | 本计划写这些文件的卡开工前逐文件取得独占写锁：独立卡持有至本卡「文档与网站验收命令」第 7 步通过；家族卡（`REL-CSR-03`/`04`/`05`/`TRACK`/`ORPHAN`）在首张子卡开工前取得家族级锁，持有至发布点第 7 步通过。两计划间只互斥、不设先后顺序边；开工时按 ER-02 刷新 `checkout.rs` 锚点 | 锁取得与释放 UTC 记入卡内并同步 `plan-status.md` | 存在其它持锁者时本卡置 `blocked` 并登记等待，禁止并发写（G-10） |
| DEP-CSR-09 | incoming | 跨计划写集串行窗口（协调者） | 全部同批 `plan-20261117-*.md` 中与本卡 `Implementation write set` 相交的在途卡。2026-11-18 按已推送计划核对的已知交集：`global-init-infra` 的 `GII-10`/`GII-11`（`src/command/status.rs`、`src/command/status/output.rs`、`tests/command/status_test.rs`、status 文档与 `status.en.md`，与 `CSR-08`、`CSR-13` 相交）与 `GII-13`/`GII-21`（`tests/command/merge_test.rs`、merge 文档与 `merge.en.md`，与 `CSR-21` 相交）；`commit-signing` 的 `CS-02a`/`CS-03c`/`CS-03d`/`CS-03g`/`CS-03h`/`CS-03i`/`CS-03k`/`CS-03l`（`src/command/merge.rs` 或 `docs/commands/merge.md`，与 `CSR-21` 相交；对方 `DEP-CS-07` 为同一协调者窗口）；`diff` 计划写 `src/command/diff.rs` 的卡（与 `CSR-27a` 相交）；以及全部网站交付共享的被忽略生成产物 `apps/tanstack-app/src/generated/docs-manifest.ts`、`docs-search.ts` 与 `apps/tanstack-app/dist/`（与对方 `DEP-GII-03`、`DEP-DESC-DOCS` 同一窗口）。未提交计划（`branch`、`clone`、`grep`、`merge-pull`、`sequencer-am`）的交集以其提交版本为准 | 仓库协调者 | 每张卡开工编辑前以 `plan-status.md` 与各计划写集核对交集：无交集即满足；有交集时取得相交文件的单写者窗口，持有至本卡合入 `main`（网站生成产物持有至第 7 步通过；家族卡持有至发布点） | 核对结果、交集文件与放行记录（起止 UTC）记入卡内 | 未满足前保持 `pending`，不编辑相交文件（G-10）；超过 7 日未获窗口报告协调者 |
| DEP-CSR-10 | outgoing | 跨计划（对方 start-to-completion 门） | `plan-20261117-commit-signing.md#CS-08`（对方 `DEP-CS-05` 引用本计划 `CSR-01`、`CSR-02`、`CSR-27`） | 本计划执行者（产出方） | 映射：三卡均为家族子卡、不单独推送，其 `src/cli.rs` 修改分别随 `CSR-25`（`REL-CSR-01`，含同家族 `CSR-23`）与 `CSR-32`（`REL-CSR-04`，含 `CSR-27a`、`CSR-03`）的 squash merge 合入 `main`，两者都合入后对方前置即满足。本次修订把 `CSR-27` 改为 `--[no-]auto-advance` 轴，它仍是 `REL-CSR-04` 中写 `src/cli.rs`（D15 表）的卡，编号含义对对方门控无影响；`CSR-23` 未列入对方 `DEP-CS-05`，但与 `CSR-01` 同次合入，不形成遗漏 | 两次 squash merge SHA 记入 `CSR-25`/`CSR-32` | 不阻塞本计划；本计划延期时对方按其 `DEP-CS-05` 超时策略处理 |
| DEP-CSR-11 | incoming（未承接） | 外部验收移交 | `plan-20261117-fetch-push.md` 的 `DEP-FP-03`（outgoing）：把 `#719` 验收 3 / `#676` 的 `checkout -b new origin` 未出生分支检出移交本计划 | 仓库维护者 | 本计划跟踪的 20 个 issue 不含 `#676`/`#719`，任何活动卡都不交付该场景；按 `DEP-FP-03` 自身回落口径 `#719` 保持 OPEN，登记为 `DEFER-CSR-03` | 本表与「非目标与延后项」 | 不阻塞本计划；如需承接须先修订本计划新增卡并重新双审 |
| DEP-CSR-12 | incoming | 跨计划实现复用 | `plan-20261117-diff.md#DIFF-09`（`--inter-hunk-context` 的 hunk 合并，写 `src/command/diff.rs`） | Diff 计划执行者 | `DIFF-09` 已合入 `main`；为免家族窗口内持锁等待，`REL-CSR-04` 首张子卡 `CSR-02` 开工前即核对，`CSR-27a` 复用其实现（GC-02），不另写第二套 | 对方卡 ID 与合入 SHA 记入 `CSR-02`/`CSR-27a` | 未合入前 `CSR-02` 起的整个 `REL-CSR-04` 保持 `pending`；超过 7 日报告协调者 |

### 发布分组与并发窗口

默认独立发布切片（G-07）；以下六组必须同批交付交互/状态边界的切片按 G-08 登记为家族卡：

| ID | 成员 | 唯一发布点 | 窗口规则 | 失败回滚顺序 | 理由 |
|---|---|---|---|---|---|
| REL-CSR-01 | `CSR-01`, `CSR-23`, `CSR-25` | `CSR-25` | `CSR-01`/`CSR-23` 本地签名提交且不推送、不 bump；窗口期禁止插入其它切片；`CSR-25` 统一跑 `T-2` 全量与 `patch + 1` 发布 | `CSR-23` → `CSR-01`（本地 `revert`）或 `CSR-25` 后 `immutable-release` 前滚 | `restore -p` 全交互键与 `--source` 扩展必须同版发布 |
| REL-CSR-03 | `CSR-08`, `CSR-28`, `CSR-31` | `CSR-31` | `CSR-08`/`CSR-28` 本地签名提交且不推送、不 bump；窗口期禁止插入其它切片；`CSR-31` 统一跑 `T-2` 全量与 `patch + 1` 发布 | `CSR-28` → `CSR-08`（本地 `revert`）或 `CSR-31` 后 `immutable-release` 前滚 | `checkout` ahead/behind 输出与 `--detach`/`--quiet`/`--json`/缺引用抑制必须同版发布 |
| REL-CSR-04 | `CSR-02`, `CSR-27`, `CSR-27a`, `CSR-03`, `CSR-32` | `CSR-32` | `CSR-02`/`CSR-27`/`CSR-27a`/`CSR-03` 本地签名提交且不推送、不 bump；窗口期禁止插入其它切片；`CSR-32` 统一跑 `T-2` 全量与 `patch + 1` 发布 | `CSR-03` → `CSR-27a` → `CSR-27` → `CSR-02`（本地 `revert`）或 `CSR-32` 后 `immutable-release` 前滚 | `checkout -p`、辅助选项、`--overlay` 默认与 `-p --overlay` 互斥必须同版发布 |
| REL-CSR-TRACK | `CSR-10`, `CSR-10a`, `CSR-10R` | `CSR-10R` | 子卡本地签名提交且不推送、不 bump；窗口期禁止插入其它切片；`CSR-10R` 统一 T-2 与 patch+1 | `CSR-10a` → `CSR-10`（本地 revert）或发布后前滚 | track 参数升级与裸 --track 兼容必须同版 |
| REL-CSR-ORPHAN | `CSR-15`, `CSR-15a`, `CSR-15R` | `CSR-15R` | 子卡本地签名提交且不推送、不 bump；窗口期禁止插入其它切片；`CSR-15R` 统一 T-2 与 patch+1 | `CSR-15a` → `CSR-15`（本地 revert）或发布后前滚 | orphan 起点功能与 dirty 保护/失败原子性必须同版 |
| REL-CSR-05 | `CSR-09`, `CSR-33`, `CSR-24`, `CSR-34` | `CSR-34` | `CSR-09`/`CSR-33`/`CSR-24` 本地签名提交且不推送、不 bump；窗口期禁止插入其它切片；`CSR-34` 统一跑 `T-2` 全量与 `patch + 1` 发布。`CSR-35` 已合回 `CSR-09`，不再占发布槽 | `CSR-24` → `CSR-33` → `CSR-09`（本地 `revert`）或 `CSR-34` 后 `immutable-release` 前滚 | `--pathspec-from-file=<file>` 与其全部互斥、`-` 标准输入与 `--pathspec-file-nul` 必须同版发布 |

- **并发声明:** 全串行（按 G-10 禁止 I–I 并发）。
- **发布者:** Plan Owner / 仓库维护者 `Eli`（作为本计划单一串行发布负责人，由执行本计划的 Agent 在其授权下串行执行各独立发布卡与家族发布点的 `patch + 1`、`gh release create`、D 组远端门证据收集及家族 `/Volumes/Data/gitmono/libra-backend` `cf` 分支统一推送；`family child` 卡仅本地验收、跑 `fmt`+`clippy` 并本地签名提交，不在发布点之前推送 `cf`）。
- **发布窗口顺序（只列发版单元；家族子卡不发版）:** `CSR-25`（REL-CSR-01）→ `CSR-32`（REL-CSR-04）→ `CSR-04` → `CSR-05` → `CSR-06` → `CSR-07` → `CSR-31`（REL-CSR-03）→ `CSR-34`（REL-CSR-05：`CSR-09`+`CSR-33`+`CSR-24`）→ `CSR-10R`（REL-CSR-TRACK：`CSR-10`+`CSR-10a`）→ `CSR-11` → `CSR-12` → `CSR-13` → `CSR-29` → `CSR-14` → `CSR-15R`（REL-CSR-ORPHAN：`CSR-15`+`CSR-15a`）→ `CSR-16` → `CSR-17` → `CSR-18` → `CSR-19` → `CSR-20` → `CSR-30` → `CSR-21` → `CSR-22`，同一时刻仅允许 1 个发布单元处于已 bump 未完成推送/发布状态（ER-12）。

### Phase 0: Patch 模式、路径恢复与参数校验（CSR-01..CSR-09, CSR-23..CSR-25, CSR-27/27a, CSR-28, CSR-31..CSR-34；CSR-35 已废弃）

**目标:** 交付 `restore -p`、`checkout -p`、`--overlay` 语义与诊断、`--ours`/`--theirs`、`--no-overlay <tag>`、`.` 消歧与进度选项、`--detach`+`-b` 互斥、追踪状态打印与 `--pathspec-from-file` / `--pathspec-file-nul`。

**进入条件:**

- 本计划文本经 Claude（Subagent）与 Codex 对同一字节双审 `VERDICT: PASS` 后提交，且 `DEP-CSR-01` 基线 parity 预检通过。

**退出条件:**

- `CSR-01`..`CSR-09`、`CSR-23`..`CSR-25`、`CSR-27`、`CSR-27a`、`CSR-28`、`CSR-31`..`CSR-34` 全部达到 `Lifecycle=done`、`Acceptance=complete`。废弃编号 `CSR-35` 不占完成门。

### Phase 1: 追踪继承、符号链接安全与合并切换（`CSR-10`、`CSR-10a`、`CSR-10R`、`CSR-11`..`CSR-15`、`CSR-15a`、`CSR-15R`、`CSR-16`、`CSR-29`）

**目标:** 交付 `--track=direct|inherit`、`-f` 符号链接安全删除、`-m` 合并切换、`status.rs` D/F 修复、`--force` 替换前导目录链接、`--orphan <start-point>` 与 unborn `-b`。

**进入条件:**

- Phase 0 退出条件满足。

**退出条件:**

- `CSR-10`、`CSR-10a`、`CSR-10R`、`CSR-11`..`CSR-15`、`CSR-15a`、`CSR-15R`、`CSR-16`、`CSR-29` 全部完成，且命中 `T-1` 的独立卡（`CSR-11`、`CSR-12`、`CSR-13`、`CSR-29`、`CSR-14`、`CSR-16`）已通过对应全量 nextest；家族子卡 CSR-10/10a/15/15a 不独立跑 T-1，由 CSR-10R/CSR-15R 的 T-2 覆盖。

### Phase 2: DWIM、`-B` 重置、D/F 冲突与 `switch -C` 收口（CSR-17 .. CSR-22, CSR-30）

**目标:** 交付多远端 DWIM 歧义、`-B` 保留暂存改动、唯一远端引用 DWIM、非空工作区目录替换、`merge` D/F stage 1/2 记录与 `switch -C` unborn HEAD，并完成收口全量门。

**进入条件:**

- Phase 1 退出条件满足。

**退出条件:**

- 38 张活动卡（`CSR-01`..`CSR-25`、`CSR-27`、`CSR-27a`、`CSR-28`..`CSR-34`、`CSR-10a`、`CSR-10R`、`CSR-15a`、`CSR-15R`，不含已废弃的 `CSR-35`）全部 `done/complete`，收口全量门全绿。

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

**规则 waiver（`EX-*`）：** 当前无活动 waiver。历史 `EX-CSR-03`（`CSR-09` 门族型 15/8，2026-11-17 08:28:29 UTC 修订历史行记录的 Plan Owner 批准）已于 2026-11-18 撤销：`CSR-09` 按 ADR-CSR-03 以稳定错误码判定失败路径后为 8/8，无需豁免。

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| CSR-01 | implementation | restore-pD15解禁与y/n/q/互斥 | revert本地提交 | yes | yes | 8/8 | 3/8 | 3/3 | M | DEP-CSR-01,DEP-CSR-02,DEP-CSR-03,DEP-CSR-05,DEP-CSR-07,DEP-CSR-09 | 串行起点 | family child | N/A | N/A |
| CSR-23 | implementation | restore-p控制键a/d/s/e与--source | revert本地提交 | yes | yes | 8/8 | 2/8 | 3/3 | M | CSR-01,DEP-CSR-02,DEP-CSR-03,DEP-CSR-05,DEP-CSR-07,DEP-CSR-09 | 序列化于CSR-01 | family child | CSR-01 | N/A |
| CSR-25 | release | REL-CSR-01家族发布点 | patch+1前滚修复 | yes | yes | 7/12 | 6/12 | 0/0 | S | CSR-23,DEP-CSR-02,DEP-CSR-03,DEP-CSR-05,DEP-CSR-07,DEP-CSR-09 | 序列化于CSR-23 | family release point | N/A | N/A |
| CSR-02 | implementation | checkout-p核心交互与互斥 | revert本地提交 | yes | yes | 7/8 | 3/8 | 3/3 | M | CSR-25,DEP-CSR-02,DEP-CSR-03,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09,DEP-CSR-12 | 序列化于CSR-25 | family child | N/A | N/A |
| CSR-27 | implementation | checkout-p导航--[no-]auto-advance | revert本地提交 | yes | yes | 8/8 | 3/8 | 2/2 | M | CSR-02,DEP-CSR-02,DEP-CSR-03,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-02 | family child | CSR-02 | N/A |
| CSR-27a | implementation | checkout-p的hunk形状-U与inter-hunk | revert本地提交 | yes | yes | 8/8 | 3/8 | 2/2 | M | CSR-27,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09,DEP-CSR-12 | 序列化于CSR-27 | family child | CSR-27 | N/A |
| CSR-03 | implementation | checkout默认overlay与诊断 | revert本地提交 | yes | yes | 8/8 | 2/8 | 1/1 | M | CSR-27a,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-27a | family child | N/A | N/A |
| CSR-32 | release | REL-CSR-04家族发布点 | patch+1前滚修复 | yes | yes | 9/12 | 8/12 | 0/0 | S | CSR-03,DEP-CSR-02,DEP-CSR-03,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-03 | family release point | N/A | N/A |
| CSR-04 | implementation | checkout--theirs/--ours选边 | patch+1前滚修复 | yes | yes | 7/8 | 2/8 | 1/1 | M | CSR-32,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-32 | independent | N/A | N/A |
| CSR-05 | implementation | --no-overlay<tag>源树解析 | patch+1前滚修复 | yes | yes | 5/8 | 2/8 | 2/2 | M | CSR-04,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-04 | independent | N/A | N/A |
| CSR-06 | implementation | .路径消歧与进度选项 | patch+1前滚修复 | yes | yes | 5/8 | 1/8 | 2/2 | M | CSR-05,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-05 | independent | N/A | N/A |
| CSR-07 | implementation | --detach与-b/-B/--orphan互斥 | patch+1前滚修复 | yes | yes | 4/8 | 1/8 | 1/1 | M | CSR-06,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-06 | independent | N/A | N/A |
| CSR-08 | implementation | checkout追踪状态文本 | revert本地提交 | yes | yes | 8/8 | 1/8 | 2/3 | M | CSR-07,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-07 | family child | N/A | N/A |
| CSR-28 | implementation | checkout追踪状态抑制与降级 | revert本地提交 | yes | yes | 7/8 | 1/8 | 1/1 | M | CSR-08,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-08 | family child | CSR-08 | N/A |
| CSR-31 | release | REL-CSR-03家族发布点 | patch+1前滚修复 | yes | yes | 7/12 | 6/12 | 0/0 | S | CSR-28,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-28 | family release point | N/A | N/A |
| CSR-09 | implementation | --pathspec-from-file=<file>读取与全部互斥 | revert本地提交 | yes | yes | 8/8 | 2/8 | 2/2 | M | CSR-31,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-31 | family child | N/A | N/A |
| CSR-33 | implementation | --pathspec-from-file=-标准输入 | revert本地提交 | yes | yes | 2/8 | 1/8 | 2/2 | M | CSR-09,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-09 | family child | CSR-09 | N/A |
| CSR-24 | implementation | --pathspec-file-nul切分与报错 | revert本地提交 | yes | yes | 4/8 | 1/8 | 2/2 | M | CSR-33,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-33 | family child | CSR-09 | N/A |
| CSR-34 | release | REL-CSR-05家族发布点 | patch+1前滚修复 | yes | yes | 8/12 | 7/12 | 0/0 | S | CSR-24,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-24 | family release point | N/A | N/A |
| CSR-10 | implementation | --track=inherit/direct与autoSetupMerge | revert本地提交 | yes | yes | 8/8 | 1/8 | 2/2 | M | CSR-34,DEP-CSR-02,DEP-CSR-06,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-34 | family child | N/A | N/A |
| CSR-10a | implementation | 裸--track的direct兼容 | revert本地提交 | yes | yes | 3/8 | 1/8 | 1/1 | M | CSR-10,DEP-CSR-02,DEP-CSR-06,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-10 | family child | CSR-10 | N/A |
| CSR-10R | release | REL-CSR-TRACK家族发布点 | patch+1前滚修复 | yes | yes | 7/12 | 6/12 | 0/0 | S | CSR-10a,DEP-CSR-02,DEP-CSR-06,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-10a | family release point | N/A | N/A |
| CSR-11 | implementation | -f逐组件符号链接不跟随与常规/索引回归 | patch+1前滚修复 | yes | yes | 6/8 | 2/8 | 3/3 | M | CSR-10R,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-10R | independent | N/A | N/A |
| CSR-12 | implementation | checkout-m合并切换与冲突恢复 | patch+1前滚修复 | yes | yes | 8/8 | 2/8 | 3/3 | M | CSR-11,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-11 | independent | N/A | N/A |
| CSR-13 | implementation | status宽容D/F与普通checkoutHEAD | patch+1前滚修复 | yes | yes | 5/8 | 1/8 | 3/3 | M | CSR-12,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-12 | independent | N/A | N/A |
| CSR-29 | implementation | --force恢复D/F与F.t | patch+1前滚修复 | yes | yes | 4/8 | 2/8 | 3/3 | M | CSR-13,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-13 | independent | CSR-13 | N/A |
| CSR-14 | implementation | --force替换前导目录符号链接 | patch+1前滚修复 | yes | yes | 6/8 | 2/8 | 3/3 | M | CSR-29,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-29 | independent | N/A | N/A |
| CSR-15 | implementation | checkout--orphan接收start-point | revert本地提交 | yes | yes | 6/8 | 1/8 | 1/1 | M | CSR-14,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-14 | family child | N/A | N/A |
| CSR-15a | implementation | orphan起点dirty保护与失败原子性 | revert本地提交 | yes | yes | 5/8 | 1/8 | 1/1 | M | CSR-15,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-15 | family child | CSR-15 | N/A |
| CSR-15R | release | REL-CSR-ORPHAN家族发布点 | patch+1前滚修复 | yes | yes | 7/12 | 6/12 | 0/0 | S | CSR-15a,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-15a | family release point | N/A | N/A |
| CSR-16 | implementation | unborn上checkout-b成功 | patch+1前滚修复 | yes | yes | 4/8 | 1/8 | 1/1 | M | CSR-15R,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-15R | independent | N/A | N/A |
| CSR-17 | implementation | 多远端DWIM歧义不回退pathspec | patch+1前滚修复 | yes | yes | 5/8 | 2/8 | 2/2 | M | CSR-16,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-16 | independent | N/A | N/A |
| CSR-18 | implementation | checkout-B保留可合并暂存与共享预检回归 | patch+1前滚修复 | yes | yes | 6/8 | 2/8 | 2/2 | M | CSR-17,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-17 | independent | N/A | N/A |
| CSR-19 | implementation | 单一远程引用DWIM解析 | patch+1前滚修复 | yes | yes | 8/8 | 1/8 | 2/2 | M | CSR-18,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-18 | independent | N/A | N/A |
| CSR-20 | implementation | 普通模式非空目录替换为跟踪文件 | patch+1前滚修复 | yes | yes | 6/8 | 2/8 | 3/3 | M | CSR-19,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-19 | independent | N/A | N/A |
| CSR-30 | implementation | --force替换非空目录与清理 | patch+1前滚修复 | yes | yes | 6/8 | 2/8 | 3/3 | M | CSR-20,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09 | 序列化于CSR-20 | independent | CSR-20 | N/A |
| CSR-21 | implementation | merge的D/F记录destdir~HEADstage1/2 | patch+1前滚修复 | yes | yes | 5/8 | 2/8 | 2/2 | M | CSR-30,DEP-CSR-02,DEP-CSR-04,DEP-CSR-07,DEP-CSR-09 | 序列化于CSR-30 | independent | N/A | N/A |
| CSR-22 | implementation | switch-C在unbornHEAD | patch+1前滚修复 | yes | yes | 4/8 | 1/8 | 1/1 | M | CSR-21,DEP-CSR-02,DEP-CSR-07,DEP-CSR-09 | 序列化于CSR-21 | independent | N/A | N/A |

---

### Task CSR-01: 实现 `libra restore -p/--patch` D15 解禁、核心 `y`/`n`/`q` 交互与互斥（#1037 部分 1/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #1037 核心交互与 D15 解禁轴（`REL-CSR-01` 子卡 1/2，经 `CSR-25` 联合发布）：删除 `src/cli.rs:3028-3037` D15 表中 `restore` 的 `-p`/`--patch` 条目（同步 `tests/compat/README.md:97` 与 `docs/development/commands/_compatibility.md` 的 D15 行），复用 `src/internal/patch_mode/` 在 `src/command/restore.rs` 落地工作区 discard 写入（`patch_mode` 自身仍不写工作区，见 `mod.rs:1-6`，写入由 `restore.rs` 调用方完成，`add -p`/`reset -p` 行为不变），实现 `y`/`n`/`q` 与 `Discard` 提示；`--staged` 在 patch 下合法并按 hunk 更新索引；`-p` 与 `--pathspec-from-file` 互斥（TC-2624）。
**Out of scope:** `a`/`d`/`s`/`e` 控制键与 `--source`（由 `CSR-23` 承接，同属 `REL-CSR-01`）；`checkout -p`（由 `CSR-02` 承接）。G-09 拆分登记：本卡拆出 `CSR-23`。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `patch_mode` 只解析、拆分、重组 hunk 并写索引 blob，不写工作区；现有调用方为 `add -p`/`reset -p` | `src/internal/patch_mode/mod.rs:1-6` |
| `restore -p`/`--patch` 在 D15 拒绝表登记（`LBR-UNSUPPORTED-001`），compat 守卫遍历该表，README 同步登记 | `src/cli.rs:3028-3037`、`tests/compat/compat_declined_interactive_flags.rs:79`、`tests/compat/README.md:97` |
| `run_restore` 先读 `--pathspec-from-file` 再编译 pathspec，无 patch 分支 | `src/command/restore.rs:462-468` |
**Acceptance criteria:**
- [ ] **AC1** `src/cli.rs` D15 拒绝表不再含 `command: "restore"` 且 `flags` 含 `-p` 或 `--patch` 的条目。
- [ ] **AC2** `libra restore -p -- a.txt` 对第 1 个 hunk 答 `y` 时把该 hunk 从工作区丢弃（恢复为索引内容）。
- [ ] **AC3** 同一次运行对第 2 个 hunk 答 `n` 时保留该 hunk 的工作区改动。
- [ ] **AC4** `libra restore -p -- a.txt` 在第 1 个 hunk 答 `q` 时不应用任何 hunk。
- [ ] **AC5** 工作区恢复的交互提示行等于 Git 期望（含 `Discard this hunk from worktree`）。
- [ ] **AC6** `libra restore -p --staged -- a.txt` 按所答 hunk（`y`、`n`）更新索引中的 `a.txt`。
- [ ] **AC7** `libra restore -p --pathspec-from-file=<f>` 以 `LBR-CLI-002` 拒绝（默认派生退出码 129）。
- [ ] **AC8** 上述拒绝的 stderr 含 Git 诊断 `options '--pathspec-from-file' and '--patch' cannot be used together`（TC-2624）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-25` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F01：已提交 `a.txt` = `1\n`…`20\n`（20 行）；工作区把第 3 行改为 `X3`、第 17 行改为 `X17`（默认 3 行上下文下为 2 个 hunk）；`--staged` 场景把同样修改 `libra add` 进索引、工作区与索引一致；`<f>` 为仓库外临时目录中的 `paths.txt`（内容 `a.txt\n`）；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具与同一 stdin，快照按第 6 项）
- AC1 → V1：遍历 `src/cli.rs:3028` 起的 `DeclinedInteractiveFlag` 表，断言不存在 `command == "restore"` 且 `flags` 含 `"-p"` 或 `"--patch"` 的条目。
- AC2 → V2：stdin `y\nn\n`；exit 0；`a.txt` 第 3 行为 `3`。
- AC3 → V2：同一次运行；`a.txt` 第 17 行为 `X17`，整份 `a.txt` 字节等于 Git 期望（`git restore -p -- a.txt`，同 stdin）。
- AC4 → V2：stdin `q\n`；exit 等于 Git 期望（0）；`S.wt` 与 `S.idx` 与运行前逐字相等；对照：同夹具 stdin `y\nn\n` 的 Git 结果改变 `a.txt`。
- AC5 → V2：AC2 那次运行 stdout 中第一个含 `Discard this hunk from worktree` 的行逐字等于 Git 期望中的对应行。
- AC6 → V2：`--staged` 场景 stdin `y\nn\n`；exit 0；`libra ls-files --stage -- a.txt` 的 blob 对象名等于 Git 期望（同内容同对象名）。
- AC7 → V3：exit 非 0，stderr 含 `Error-Code: LBR-CLI-002`，`S` 不变。
- AC8 → V3：同一次调用 stderr 含 `options '--pathspec-from-file' and '--patch' cannot be used together`。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --lib -E 'test(=cli::tests::declined_interactive_table_omits_restore_patch)'`（卡特有 `(new)`；覆盖 AC1；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::restore_test::restore_patch_mode)'`（卡特有 `(new)`；覆盖 AC2–AC6；按「验证执行协议」第 1–4 项）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::restore_test::restore_patch_rejects_pathspec_from_file)'`（卡特有 `(new)`；覆盖 AC7–AC8；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test compat_declined_interactive_flags`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::restore_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::add_patch_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-25` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** DEP-CSR-01, DEP-CSR-02, DEP-CSR-03, DEP-CSR-05, DEP-CSR-07, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/cli.rs`, `src/internal/patch_mode/`, `src/command/restore.rs`, `tests/command/restore_test.rs`, `tests/compat/compat_declined_interactive_flags.rs`, `tests/compat/README.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `docs/development/commands/_compatibility.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/restore.md`、`docs/commands/zh-CN/restore.md`、`docs/development/commands/restore.md`（`-p`/`--patch` 交互恢复、`y`/`n`/`q`、`--staged`、与 `--pathspec-from-file` 互斥）、`docs/development/commands/_compatibility.md`（D15 行移除 `restore -p`）、`tests/compat/README.md`（`compat_declined_interactive_flags` 行移除 `restore -p`）、网站 `restore.en.md`（由 `sync-libra-command-docs.py --only restore` 生成）、`COMPATIBILITY.md`（W-1 定位 `restore` 行原地更新）。文档关键字（EN/zh 命令页）：`restore -p`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（3 个落点，3 个生产文件；单一公开交互接口）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-01 (1/2)
**C/D coverage from:** CSR-25
**Granularity:** `type=implementation; axis=restore-pD15解禁与y/n/q/互斥; recovery=revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=3; prod-files=3; scope=M; deps=DEP-CSR-01,DEP-CSR-02,DEP-CSR-03,DEP-CSR-05,DEP-CSR-07,DEP-CSR-09; writeset=串行起点; release=family child; split-from=N/A; exception=N/A`

---

### Task CSR-23: 补齐 `restore -p` `a`/`d`/`s`/`e` 控制键与 `--source` 恢复（#1037 部分 2/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #1037 扩展控制键与 `--source` 恢复轴（G-09：自 `CSR-01` 拆出；`REL-CSR-01` 子卡 2/2，经 `CSR-25` 联合发布）：在 `CSR-01` 的 `restore -p` 会话上接通 `a`/`d`/`s`/`e` 控制键（复用 `patch_mode` 的拆分与 `edit.rs` 编辑器流程），并支持 `--source=<rev>`（含 `HEAD^...` merge-base 写法）与 `-- <dir>` 目录限定。
**Out of scope:** D15 解禁、`y`/`n`/`q`、`--staged` 与 `--pathspec-from-file` 互斥（`CSR-01` 已交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `patch_mode` 已实现 `s` 拆分与 `e` 编辑（`add -p` 使用），`restore` 尚未接通 | `src/internal/patch_mode/mod.rs:1-6`、`src/internal/patch_mode/edit.rs:175`、`src/command/restore.rs:462-480` |
**Acceptance criteria:**
- [ ] **AC1** `libra restore -p -- a.txt b.txt` 在 `a.txt` 首个 hunk 答 `a` 时丢弃 `a.txt` 当前及其后全部 hunk。
- [ ] **AC2** 同夹具在 `a.txt` 首个 hunk 答 `d` 时不丢弃 `a.txt` 任何 hunk 并转入 `b.txt`。
- [ ] **AC3** 对可拆分 hunk 答 `s` 后按拆出的小 hunk 逐个确认。
- [ ] **AC4** 答 `e` 时按编辑器修改后的 hunk 应用。
- [ ] **AC5** `libra restore -p --source=HEAD -- c.txt` 以 `HEAD` 为源交互恢复（TC-2607）。
- [ ] **AC6** `libra restore -p --source=HEAD^ -- c.txt` 以 `HEAD^` 为源交互恢复（TC-2608）。
- [ ] **AC7** `libra restore -p --source=side... -- c.txt` 以 `side` 与 `HEAD` 的 merge-base 为源交互恢复。
- [ ] **AC8** `libra restore -p --source=HEAD^ -- dir` 只对 `dir/` 下的变更文件提示（TC-2614）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-25` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F23：已提交 `a.txt`（20 行 `1`…`20`）与 `b.txt`=`b\n`；工作区 `a.txt` 第 3、17 行改为 `X3`、`X17`（2 个 hunk），`b.txt` 改为 `B\n`；`s` 场景另有已提交 `s.txt`（10 行 `1`…`10`），工作区改第 3、5 行（默认上下文下为 1 个可拆 hunk）；`e` 场景 `GIT_EDITOR` 为测试临时目录中的 `ed.sh`：`awk '{ if ($0=="+X3") print " X3"; else print }' "$1" > "$1.t" && mv "$1.t" "$1"`；`--source` 场景历史 C1（`c.txt`=`v1\n`、`dir/x.txt`=`x1\n`、`top.txt`=`t1\n`）→ C2=`HEAD`（三者改为 `v2`/`x2`/`t2`），另有自 C1 分出的分支 `side`（提交 C3 把 `c.txt` 改为 `s3\n`），工作区三者改为 `v3`/`x3`/`t3`；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具与同一 stdin/编辑器脚本，快照按第 6 项）
- AC1 → V1：stdin `a\nn\n`；exit 0；`a.txt` 与 `b.txt` 字节等于 Git 期望（`a.txt` 回到已提交内容、`b.txt` 仍为 `B\n`）。
- AC2 → V1：stdin `d\ny\n`；exit 0；`a.txt` 与 `b.txt` 字节等于 Git 期望（`a.txt` 保持运行前字节、`b.txt` 恢复为 `b\n`）。
- AC3 → V1：`libra restore -p -- s.txt`，stdin `s\ny\nn\n`；exit 0；`s.txt` 字节等于 Git 期望（第 3 行恢复、第 5 行保留）；对照：Git 对同夹具 stdin `y\n` 的结果不同。
- AC4 → V1：`GIT_EDITOR=<ed.sh>`，stdin `e\nn\n`；exit 0；`a.txt` 字节等于 Git 期望；对照：Git 对同夹具 stdin `y\nn\n` 的结果不同。
- AC5 → V2：stdin `y\n`；exit 0；`c.txt` 字节等于 Git 期望（`v2\n`）。
- AC6 → V2：stdin `y\n`；exit 0；`c.txt` 字节等于 Git 期望（`v1\n`）；对照：AC5 的 Git 期望不同。
- AC7 → V2：stdin `y\n`；exit 0；`c.txt` 字节等于 Git 期望（`side...` 即 `side...HEAD`，解析为 merge-base C1，结果 `v1\n`）；对照：Git 对同夹具 `--source=side` 的结果为 `s3\n`。
- AC8 → V2：stdin `y\n`；exit 0；stdout 含 `dir/x.txt` 且不含 `top.txt`；`dir/x.txt` 与 `top.txt` 字节等于 Git 期望（`top.txt` 仍为 `t3\n`）。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::restore_test::restore_patch_controls)'`（卡特有 `(new)`；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::restore_test::restore_patch_source)'`（卡特有 `(new)`；覆盖 AC5–AC8；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::restore_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::add_patch_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-25` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-01, DEP-CSR-02, DEP-CSR-03, DEP-CSR-05, DEP-CSR-07, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/cli.rs`, `src/internal/patch_mode/`, `src/command/restore.rs`, `tests/command/restore_test.rs`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/restore.md`、`docs/commands/zh-CN/restore.md`、`docs/development/commands/restore.md`（`a`/`d`/`s`/`e` 控制键、`-p --source=<rev>` 与目录限定）、网站 `restore.en.md`（由同步脚本生成）、`COMPATIBILITY.md`（W-1 定位 `restore` 行原地更新）。文档关键字（EN/zh 命令页）：`restore -p --source`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（3 个落点，3 个生产文件；单一公开交互接口）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-01 (2/2)
**C/D coverage from:** CSR-25
**Granularity:** `type=implementation; axis=restore-p控制键a/d/s/e与--source; recovery=revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-01,DEP-CSR-02,DEP-CSR-03,DEP-CSR-05,DEP-CSR-07,DEP-CSR-09; writeset=序列化于CSR-01; release=family child; split-from=CSR-01; exception=N/A`

---

### Task CSR-25: `REL-CSR-01` (`restore -p` 完整交互与互斥) 家族发布点

**Task type:** release
**Lifecycle / Acceptance:** pending / 空
**Description:** 聚合 `CSR-01`, `CSR-23`（`REL-CSR-01`），在全部子卡本地签名提交并通过本地 A/B、ER-05 与 fmt/clippy 后，统一执行 C 组（`patch + 1`、版本面三处同步、工具链刷新 `Cargo.lock`、T-2 全量、签名提交、PR 与 squash merge、`gh release create`）、D 组与家族网站第 5–7 步，作为该家族唯一发布点（G-08）。
**Out of scope:** 不引入新产品代码或文档内容改动。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CSR-01`, `CSR-23` 同属一个公开行为面，须同版发布 | 「发布分组与并发窗口」`REL-CSR-01` |
**Acceptance criteria:**
- [ ] **AC1** `CSR-01` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC2** `CSR-23` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC3** 聚合守卫：家族全部卡特有用例（下方「聚合守卫」所列）在已 bump 的发布树上逐条按「验证执行协议」第 1–3 项通过。
- [ ] **AC4** `gh release view v<version>` 的 release note 正文逐条列出 `CSR-01` 与 `CSR-23` 的用户可见变化。
- [ ] **AC5** 每张子卡在「文档与网站验收命令」第 1 步登记的每个仓库文档改动后 SHA-256，等于该子卡本地签名提交中同一文件的 SHA-256。
- [ ] **AC6** 每个被家族子卡修改的仓库文档在发布树中的 SHA-256，等于家族顺序中最后修改它的子卡的登记值。
- [ ] **AC7** 家族网站页 `restore.en.md` 按「文档与网站验收命令」第 2–7 步完成（第 3 步以本卡合入 `main` 后的家族最终 EN 文档同步，第 8 项家族口径）。
（ER-04 的 fmt / clippy / T-2 全量与 ER-08 版本面门按 G-03 不计入本卡特有 AC/VER 分子。）
**AC → 断言映射:**（非测试型判据按「验证执行协议」第 9 项；`<REL>` 为本卡 C 组第⑦步签名提交 SHA，`<子卡 提交>` 为各子卡本地签名提交 SHA（登记于子卡记录））
- AC1 → V1：`libra merge-base --is-ancestor <CSR-01 提交> <REL>` exit 0。
- AC2 → V2：`libra merge-base --is-ancestor <CSR-23 提交> <REL>` exit 0。
- AC3 → 聚合守卫：所列每条命令按协议第 1–3 项判定全部通过（ER-04 B 组 `release` 类型门），逐条记录 exit code 与计数。
- AC4 → V3：`gh release view v<version> --json body --jq .body` exit 0，正文对 `CSR-01` 与 `CSR-23` 各有一条变化说明（逐条以卡 ID 标注）。
- AC5 → V4：对每张子卡登记的每个文档执行 `libra show <该子卡提交>:<路径> | shasum -a 256`，逐一等于其登记值；任一不等即失败。
- AC6 → V5：对每个文档取最后修改它的子卡登记值，与 `libra show <REL>:<路径> | shasum -a 256` 比较相等；任一不等即失败。
- AC7 → V6：第 2–7 步逐步记录命令、退出码与 SHA-256；任一步 STOP 即不成立。
**Verification:**
- [ ] V1 `libra merge-base --is-ancestor <CSR-01 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V2 `libra merge-base --is-ancestor <CSR-23 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V3 `gh release view v<version> --json body --jq .body`（非测试型，按协议第 9 项；exit 0 且逐卡说明齐全为通过）
- [ ] V4 `libra show <子卡提交>:<路径> | shasum -a 256`（逐子卡逐文件；非测试型，按协议第 9 项）
- [ ] V5 `libra show <REL>:<路径> | shasum -a 256`（逐文件；非测试型，按协议第 9 项）
- [ ] V6 「文档与网站验收命令」第 2–7 步（非测试型，按协议第 9 项；各步判据见该节）
**聚合守卫（ER-04 B 组 `release` 类型门，不计入 VER）：** `source .env.test && cargo nextest run --lib -E 'test(=cli::tests::declined_interactive_table_omits_restore_patch)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::restore_test::restore_patch_mode)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::restore_test::restore_patch_rejects_pathspec_from_file)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::restore_test::restore_patch_controls)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::restore_test::restore_patch_source)'`；共享回归组 `source .env.test && cargo nextest run --test compat_declined_interactive_flags`
**Full-suite trigger:** T-2：`family release point`（C 组第④步在 bump 后的精确提交树上跑 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 与 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，记录提交 SHA 与 `HEAD^{tree}`；合入与发布前核对发布树与已测树一致，变化即重跑）
**Dependencies:** CSR-23, DEP-CSR-02, DEP-CSR-03, DEP-CSR-05, DEP-CSR-07, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `docs/development/plan/plan-20261117-checkout-switch-restore.md`（发布证据登记）, `tests/INDEX.md`（仅需同步时）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`（第 6 步提交）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 版本面三处（`Cargo.toml`、`install.sh`、`install.ps1`）与工具链刷新的 `Cargo.lock`（属 `Release write set`）
**Docs and compatibility impact:** 不新增文档内容：汇总子卡文档交付证据（AC5、AC6），以家族最终 EN 文档完成网站 `restore.en.md` 第 2–7 步（AC7）。
**Rollback mode:** immutable-release
**Estimated scope:** S（0 个生产文件，仅家族发布聚合）
**Version increment:** patch
**Release boundary:** family release point of REL-CSR-01
**C/D coverage from:** self
**Granularity:** `type=release; axis=REL-CSR-01家族发布点; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/12; VER=6/12; landing=0; prod-files=0; scope=S; deps=CSR-23,DEP-CSR-02,DEP-CSR-03,DEP-CSR-05,DEP-CSR-07,DEP-CSR-09; writeset=序列化于CSR-23; release=family release point; split-from=N/A; exception=N/A`

---

### Task CSR-02: 实现 `libra checkout -p/--patch` 核心交互检出与互斥校验（#702 部分 1/4）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #702 中 `-p`/`--patch` 核心交互轴（`REL-CSR-04` 子卡 1/4，经 `CSR-32` 联合发布）：在 `CheckoutArgs` 注册 `-p`/`--patch`，删除 `src/cli.rs:3039-3048` D15 表的 `checkout -p` 条目（同步 `tests/compat/README.md:97` 与 `_compatibility.md` D15 行），复用 `src/internal/patch_mode/` 与 `CSR-01` 落地的 restore 工作区写入实现「从索引」与「从 `<rev>`」两种 patch 检出；`-p` 与 `-b`/`-B`/`--orphan` 联用经现有路径模式守卫拒绝。
**Out of scope:** `--auto-advance`/`--no-auto-advance`（`CSR-27`）；`-U`/`--inter-hunk-context`（`CSR-27a`）；`--overlay` 默认与 `-p --overlay` 诊断（`CSR-03`）。G-09 拆分登记：本卡拆出 `CSR-27`。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `checkout -p` 在 D15 表登记，compat 守卫遍历该表，README 同步登记 | `src/cli.rs:3039-3048`、`tests/compat/compat_declined_interactive_flags.rs:79`、`tests/compat/README.md:97` |
| `CheckoutArgs` 无 `-p`；路径模式已以 `InvalidPathMode`（`LBR-CLI-002`）拒绝 `-b`/`-B`/`--orphan` | `src/command/checkout.rs:53-120,706-716,243-249` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout -p -- a.txt` 按所答 hunk（`y` 应用、`n` 保留）从索引恢复工作区 `a.txt`。
- [ ] **AC2** `libra checkout -p HEAD -- a.txt` 按所答 hunk 同时更新索引与工作区中的 `a.txt`。
- [ ] **AC3** `libra checkout -p C0 -- a.txt`（非 `HEAD` 的提交）按所答 hunk 同时更新索引与工作区中的 `a.txt`。
- [ ] **AC4** `libra checkout -p -b x -- a.txt` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC5** `libra checkout -p -B x -- a.txt` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC6** `libra checkout -p --orphan x -- a.txt` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC7** `src/cli.rs` D15 拒绝表不再含 `command: "checkout"` 且 `flags` 含 `-p` 或 `--patch` 的条目。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-32` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F02：C0 含 `a.txt`（20 行 `1`…`20`）；AC1、AC2 在 C0=`HEAD` 上构造：AC1 工作区第 3、17 行改为 `X3`、`X17`，索引同 `HEAD`；AC2 把第 3 行改为 `X3` 并 `libra add`，再只在工作区把第 17 行改为 `X17`（索引、工作区与 `HEAD` 三者各不相同，默认上下文下为 2 个 hunk）；AC3 在 C0 之上再提交 C1（第 3、17 行改为 `H3`、`H17`）并保持干净，以 `C0` 为源；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具与同一 stdin，快照按第 6 项）
- AC1 → V2：stdin `y\nn\n`；exit 0；`a.txt` 字节等于 Git 期望（`git checkout -p -- a.txt`：第 3 行恢复为 `3`、第 17 行保留 `X17`）。
- AC2 → V2：stdin `y\nn\n`；exit 0；`libra ls-files --stage -- a.txt` 的 blob 对象名与工作区 `a.txt` 字节同为 Git 期望（`git checkout -p HEAD -- a.txt`）中的值；对照：AC1 场景的 Git 结果不改索引。
- AC3 → V2：stdin `y\nn\n`；exit 0；`libra ls-files --stage -- a.txt` 的 blob 对象名与工作区 `a.txt` 字节同为 Git 期望（`git checkout -p C0 -- a.txt`：第 3 行回到 `3`、第 17 行保留 `H17`）；对照：Git 对同夹具 stdin `n\ny\n` 的结果不同。
- AC4 → V3：exit 非 0，stderr 含 `Error-Code: LBR-CLI-002`，`S` 不变（`S.ref` 无 `refs/heads/x`）。
- AC5 → V3：`-B x` 场景，判定同 AC4。
- AC6 → V3：`--orphan x` 场景，判定同 AC4（`HEAD` 仍指向原分支）。
- AC7 → V1：遍历 D15 表，断言不存在 `command == "checkout"` 且 `flags` 含 `"-p"` 或 `"--patch"` 的条目。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --lib -E 'test(=cli::tests::declined_interactive_table_omits_checkout_patch)'`（卡特有 `(new)`；覆盖 AC7；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_patch_mode)'`（卡特有 `(new)`；覆盖 AC1–AC3；按「验证执行协议」第 1–4 项）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_patch_rejects_branch_creation)'`（卡特有 `(new)`；覆盖 AC4–AC6；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test compat_declined_interactive_flags`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::checkout::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-32` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-25, DEP-CSR-02, DEP-CSR-03, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09, DEP-CSR-12
**Deliverables:** N/A
**Implementation write set:** `src/cli.rs`, `src/internal/patch_mode/`, `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `tests/compat/compat_declined_interactive_flags.rs`, `tests/compat/README.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/development/commands/_compatibility.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`-p`/`--patch` 从索引或 `<rev>` 交互检出、与 `-b`/`-B`/`--orphan` 互斥）、`docs/development/commands/_compatibility.md`（D15 行移除 `checkout -p`）、`tests/compat/README.md`、网站 `checkout.en.md`（由同步脚本生成）、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`checkout -p`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（3 个落点，3 个生产文件；单一公开交互接口）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-04 (1/4)
**C/D coverage from:** CSR-32
**Granularity:** `type=implementation; axis=checkout-p核心交互与互斥; recovery=revert本地提交; complete=yes; self-contained=yes; AC=7/8; VER=3/8; landing=3; prod-files=3; scope=M; deps=CSR-25,DEP-CSR-02,DEP-CSR-03,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09,DEP-CSR-12; writeset=序列化于CSR-25; release=family child; split-from=N/A; exception=N/A`

---

### Task CSR-27: 解禁并实现 `checkout -p` 的 `--auto-advance`/`--no-auto-advance`（#702 部分 2/4）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #702 中 patch 会话导航轴（G-09：自 `CSR-02` 拆出；`REL-CSR-04` 子卡 2/4）：删除 `src/cli.rs:3050-3059`、`:3061-3070` D15 表对 `checkout --no-auto-advance`/`--auto-advance` 的拒绝条目，以新单测取代 `src/cli.rs:5370` 的 `declined_interactive_table_maps_checkout_no_auto_advance`；在 `CheckoutArgs` 注册两选项（后出现者为准）并传入 `patch_mode` 会话的 `auto_advance`（`reset -p` 已有同型接线）；`--no-auto-advance` 未带 `-p` 时按 Git 拒绝，`--auto-advance` 未带 `-p` 时按 Git 接受。
**Out of scope:** `-U`/`--inter-hunk-context`（`CSR-27a`）；`--overlay`（`CSR-03`）。G-09 拆分登记：本卡拆出 `CSR-27a`。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| D15 表把 `checkout --no-auto-advance`/`--auto-advance` 视为不支持，由单测守卫 | `src/cli.rs:3050-3070,5370` |
| `patch_mode` 会话已支持 `auto_advance`（`reset -p` 接线、`add -p --no-auto-advance` 矩阵用例） | `src/command/reset.rs:127-133,224-231,327`、`tests/command/add_patch_test.rs:290` |
| Git：`--no-auto-advance` 未带 `--patch` 时 die；`--auto-advance` 为默认开启的布尔选项 | `git@6de20f6092:builtin/checkout.c:1927-1928,2127-2128` |
**Acceptance criteria:**
- [ ] **AC1** `src/cli.rs` D15 拒绝表不再含 `checkout` 的 `--auto-advance` 与 `--no-auto-advance` 条目。
- [ ] **AC2** `libra checkout -p --no-auto-advance -- a.txt b.txt` 的交互转写（stdout）等于 Git 期望。
- [ ] **AC3** `libra checkout -p --auto-advance -- a.txt b.txt` 的交互转写（stdout）等于 Git 期望。
- [ ] **AC4** `libra checkout --no-auto-advance other`（无 `-p`）以 `LBR-CLI-002` 拒绝。
- [ ] **AC5** `libra checkout --auto-advance other`（无 `-p`）按普通分支检出切换到 `other`。
- [ ] **AC6** AC2 的 `--no-auto-advance` 会话结束后，已选 hunk 写入后的 `a.txt` 与 `b.txt` 字节等于 Git 期望。
- [ ] **AC7** AC3 的 `--auto-advance` 会话结束后，`a.txt` 与 `b.txt` 字节等于 Git 期望。
- [ ] **AC8** `libra checkout -p --no-auto-advance --auto-advance -- a.txt b.txt` 以后出现者为准，交互转写等于 Git 期望。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-32` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F27：已提交 `a.txt` 与 `b.txt`（各 10 行 `1`…`10`），工作区分别把第 3 行改为 `XA`、`XB`（各 1 个 hunk）；另有与 `main` 同提交的分支 `other`；AC2/AC3 使用同一 stdin `y\n>\ny\nq\n`（两个文件的 hunk 各答一次 `y`，含一次 `>` 导航）；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具与同一 stdin，快照按第 6 项）
- AC1 → V1：遍历 D15 表，断言不存在 `command == "checkout"` 且 `flags` 含 `"--auto-advance"` 或 `"--no-auto-advance"` 的条目。
- AC2 → V2：exit 等于 Git 期望退出码；stdout 逐字等于 Git 期望。
- AC3 → V2：exit 等于 Git 期望退出码；stdout 逐字等于 Git 期望；对照：Git 在 AC2 与 AC3 的 stdout 不同。
- AC4 → V3：exit 非 0，stderr 含 `Error-Code: LBR-CLI-002`，`S` 不变。
- AC5 → V3：exit 0；`S.ref` 中 `HEAD` 指向 `refs/heads/other`。
- AC6 → V2：AC2 同一次调用后 `a.txt`、`b.txt` 字节等于 Git 期望（`--no-auto-advance` 在会话结束时统一应用所选 hunk，`add-patch.c:1990-2035`）；`S.idx` 按路径恢复边界不变。
- AC7 → V2：AC3 同一次调用后 `a.txt`、`b.txt` 字节等于 Git 期望。
- AC8 → V2：同一 stdin；exit 等于 Git 期望退出码；stdout 逐字等于 Git 期望（与 AC3 的 Git 转写相同、与 AC2 不同）。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --lib -E 'test(=cli::tests::declined_interactive_table_omits_checkout_auto_advance)'`（卡特有 `(new)`；覆盖 AC1；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_patch_auto_advance_behavior)'`（卡特有 `(new)`；覆盖 AC2–AC3、AC6–AC8；按「验证执行协议」第 1–4 项）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_auto_advance_requires_patch)'`（卡特有 `(new)`；覆盖 AC4–AC5；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test compat_declined_interactive_flags`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-32` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-02, DEP-CSR-02, DEP-CSR-03, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/cli.rs`, `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `tests/compat/compat_declined_interactive_flags.rs`, `tests/compat/README.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/development/commands/_compatibility.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`--[no-]auto-advance` 语义及其对 `-p` 的依赖）、`docs/development/commands/_compatibility.md`（D15 行移除两选项）、`tests/compat/README.md`、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--no-auto-advance`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（2 个落点，2 个生产文件；单一公开选项面）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-04 (2/4)
**C/D coverage from:** CSR-32
**Granularity:** `type=implementation; axis=checkout-p导航--[no-]auto-advance; recovery=revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=2; prod-files=2; scope=M; deps=CSR-02,DEP-CSR-02,DEP-CSR-03,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-02; release=family child; split-from=CSR-02; exception=N/A`

---

### Task CSR-27a: 支持 `checkout -p` 的 `-U<n>`/`--unified` 与 `--inter-hunk-context`（#702 部分 3/4）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #702 中 hunk 形状轴（G-09：自 `CSR-27` 拆出；`REL-CSR-04` 子卡 3/4）：在 `CheckoutArgs` 注册 `-U<n>`/`--unified=<n>` 与 `--inter-hunk-context=<n>`，把上下文行数与 hunk 间合并阈值传入 `checkout -p` 的 hunk 生成；hunk 合并复用 `plan-20261117-diff.md#DIFF-09` 在 `src/command/diff.rs` 落地的同一实现（GC-02，经 `DEP-CSR-12`），不另写第二套；两选项未带 `-p` 时按 Git 拒绝。
**Out of scope:** `--[no-]auto-advance` 与 D15 解禁（`CSR-27`）；`--overlay`（`CSR-03`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `checkout -p` 所用的 hunk 生成固定 3 行上下文（`reset -p` 同型调用），`compute_unified_hunks` 不接受 inter-hunk 参数 | `src/command/reset.rs:535`、`src/command/diff.rs:4654-4659` |
| Git：`--unified`/`--inter-hunk-context` 未带 `--patch` 时 die | `git@6de20f6092:builtin/checkout.c:1922-1926` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout -p -U2 -- a.txt` 显示 2 个 hunk，各带 2 行上下文。
- [ ] **AC2** `libra checkout -p -U2 --inter-hunk-context=5 -- a.txt` 把两处修改合并为 1 个 hunk。
- [ ] **AC3** `libra checkout -p -U2 --inter-hunk-context=4 -- a.txt` 仍显示 2 个 hunk。
- [ ] **AC4** `libra checkout -p -U0 -- a.txt` 显示 2 个不带上下文行的 hunk。
- [ ] **AC5** `libra checkout -p -U2 -- a.txt` 只对第 1 个 hunk 答 `y` 时恢复第 5 行并保留第 15 行修改。
- [ ] **AC6** `libra checkout -U2 -- a.txt`（无 `-p`）以 `LBR-CLI-002` 拒绝。
- [ ] **AC7** `libra checkout --inter-hunk-context=5 -- a.txt`（无 `-p`）以 `LBR-CLI-002` 拒绝。
- [ ] **AC8** `libra checkout -p -U0 -- a.txt` 只对第 1 个 hunk 答 `y` 时恢复第 5 行并保留第 15 行修改。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-32` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F27a：已提交 `a.txt` = `1\n`…`30\n`（30 行），工作区第 5 行改为 `X5`、第 15 行改为 `X15`（两处修改之间 9 行未改动；`-U2` 下两 hunk 之间余 5 行未改动）；「hunk 头序列」= 按出现顺序提取 stdout 中所有匹配正则 `@@ -[0-9]+(,[0-9]+)? \+[0-9]+(,[0-9]+)? @@` 的子串（不要求位于行首：管道 stdin 下提示行不换行，下一个 hunk 头会紧接在提示之后，`git@6de20f6092:add-patch.c:1678-1687,1745-1761`、`prompt.c:79-82`）；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具与同一 stdin，快照按第 6 项）
- AC1 → V1：stdin `n\nn\n`；exit 0；hunk 头序列逐字等于 Git 期望（`@@ -3,5 +3,5 @@`、`@@ -13,5 +13,5 @@`）。
- AC2 → V1：stdin `n\n`；exit 0；hunk 头序列逐字等于 Git 期望（单个 `@@ -3,15 +3,15 @@`）；对照：AC3 的 Git 期望为 2 个 hunk。
- AC3 → V1：stdin `n\nn\n`；exit 0；hunk 头序列逐字等于 Git 期望（同 AC1 的 2 个 hunk 头）。
- AC4 → V1：stdin `n\nn\n`；exit 0；hunk 头序列逐字等于 Git 期望（`@@ -5 +5 @@`、`@@ -15 +15 @@`）。
- AC5 → V2：stdin `y\nn\n`；exit 0；`a.txt` 字节等于 Git 期望（第 5 行为 `5`、第 15 行为 `X15`）。
- AC6 → V3：exit 非 0，stderr 含 `Error-Code: LBR-CLI-002`，`S` 不变。
- AC7 → V3：`--inter-hunk-context=5` 场景，判定同 AC6。
- AC8 → V2：stdin `y\nn\n`；exit 0；`a.txt` 字节等于 Git 期望（第 5 行为 `5`、第 15 行为 `X15`；覆盖零上下文 hunk 的应用）。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_patch_hunk_context)'`（卡特有 `(new)`；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_patch_selected_hunk_preserves_other)'`（卡特有 `(new)`；覆盖 AC5、AC8；按「验证执行协议」第 1–4 项）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_patch_context_requires_patch)'`（卡特有 `(new)`；覆盖 AC6–AC7；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::checkout::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-32` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-27, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09, DEP-CSR-12
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/diff.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`-U<n>`/`--unified` 与 `--inter-hunk-context` 仅用于 `-p`）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--inter-hunk-context`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（2 个落点，2 个生产文件；单一公开选项面）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-04 (3/4)
**C/D coverage from:** CSR-32
**Granularity:** `type=implementation; axis=checkout-p的hunk形状-U与inter-hunk; recovery=revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=2; prod-files=2; scope=M; deps=CSR-27,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09,DEP-CSR-12; writeset=序列化于CSR-27; release=family child; split-from=CSR-27; exception=N/A`

---

### Task CSR-03: 对齐 `checkout` 默认 `overlay` 语义与 `-p --overlay` 冲突诊断（#702 部分 4/4）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #702 中 `--overlay` 部分（ADR-CSR-02；`REL-CSR-04` 子卡 4/4）：`checkout <tree-ish> -- <pathspec>` 默认改为 Git overlay 语义（不删除源树中缺失的已跟踪路径），注册 `--overlay` 并与 `--no-overlay` 以后出现者为准；`-p --overlay` 与分支切换携带 `--overlay` 或 `--no-overlay` 时按 Git 拒绝。
**Out of scope:** `-p` 交互流程（`CSR-02`）；`--no-overlay <tag>` 的 tag 解析（`CSR-05`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `restore_checkout_paths` 构造 `RestoreArgs` 时取 no-overlay 行为，`--no-overlay` 帮助文本自称空操作，无 `--overlay` | `src/command/checkout.rs:100-107,706-722` |
| Git：`-p` 与 `--overlay` 互斥；切换分支携带 `--[no]-overlay` 报错 | `git@6de20f6092:builtin/checkout.c:1952-1953,1693-1695` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout <C1> -- d` 默认（overlay）保留源树中不存在的已跟踪路径 `d/new.txt`。
- [ ] **AC2** `libra checkout --no-overlay <C1> -- d` 从工作区与索引删除 `d/new.txt`。
- [ ] **AC3** `libra checkout --no-overlay --overlay <C1> -- d` 以后者为准，结果同 AC1。
- [ ] **AC4** `libra checkout --overlay --no-overlay <C1> -- d` 以后者为准，结果同 AC2。
- [ ] **AC5** `libra checkout -p --overlay -- d` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC6** 上述拒绝的 stderr 含 Git 诊断 `options '-p' and '--overlay' cannot be used together`。
- [ ] **AC7** `libra checkout --overlay -b foo` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC8** `libra checkout --no-overlay -b foo` 以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-32` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F03：C1 含 `d/a.txt`=`1\n`，`<C1>` 为其完整对象名（tag 作源由 `CSR-05` 承接，本卡不依赖 tag 解析）；C2=`HEAD` 把 `d/a.txt` 改为 `2\n` 并新增已跟踪 `d/new.txt`=`N\n`，工作区与索引干净；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 0；`S.wt` 与 `S.idx` 等于 Git 期望（`git checkout <C1> -- d`：`d/a.txt` 回到 `1`、`d/new.txt` 仍在）；对照：AC2 的 Git 期望不含 `d/new.txt`。
- AC2 → V1：exit 0；`S.wt` 与 `S.idx` 等于 Git 期望（`git checkout --no-overlay <C1> -- d`）。
- AC3 → V1：exit 0；`S.wt` 与 `S.idx` 等于 AC1 的 Git 期望。
- AC4 → V1：exit 0；`S.wt` 与 `S.idx` 等于 AC2 的 Git 期望。
- AC5 → V2：exit 非 0，stderr 含 `Error-Code: LBR-CLI-002`，`S` 不变。
- AC6 → V2：AC5 同一次调用的 stderr 含 `options '-p' and '--overlay' cannot be used together`。
- AC7 → V2：exit 非 0，stderr 含 `Error-Code: LBR-CLI-002`，`S` 不变（无 `refs/heads/foo`）。
- AC8 → V2：判定同 AC7（Git 对显式 `--no-overlay` 的分支切换同样拒绝，`checkout.c:1693-1695`）。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_overlay_default_and_toggle)'`（卡特有 `(new)`；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_overlay_rejections)'`（卡特有 `(new)`；覆盖 AC5–AC8；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::checkout::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-32` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-27a, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（默认 overlay、`--overlay`/`--no-overlay` 后者为准、`-p --overlay` 与切换分支时的拒绝）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--overlay`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（1 个落点，1 个生产文件；默认语义变更）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-04 (4/4)
**C/D coverage from:** CSR-32
**Granularity:** `type=implementation; axis=checkout默认overlay与诊断; recovery=revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CSR-27a,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-27a; release=family child; split-from=N/A; exception=N/A`

---

### Task CSR-04: 支持 `checkout --theirs` / `--ours` 合并端选项（#703）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #703：在 `CheckoutArgs` 注册 `--ours`（`-2`）与 `--theirs`（`-3`），复用 `restore` 的冲突阶段写出逻辑（`RestoreArgs.ours/theirs`），含 `--theirs --no-overlay` 对无 stage 3 路径删除工作区文件；与 `<tree-ish>`、`-b`、`-B` 联用时按 Git 拒绝。
**Out of scope:** `-m`/`--merge`（`CSR-12`）；拒绝路径按 Git 的两个 die 分支各取一例：与 tree-ish 联用（`checkout.c:590-592`，以 `--ours HEAD` 覆盖，`--theirs HEAD` 同分支）与切换分支联用（`checkout.c:1697-1704`，以 `--theirs -b` 覆盖，`-B` 与 `--ours` 同分支）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CheckoutArgs` 缺少 `--ours`/`--theirs`，`RestoreArgs` 已支持 | `src/command/checkout.rs:53-120`、`src/command/restore.rs:287` |
| Git：`--ours/--theirs` 与 tree-ish 联用、与切换分支联用分别 die | `git@6de20f6092:builtin/checkout.c:590-592,1697-1704` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout --ours -- f.txt` 把 stage 2 内容写入工作区 `f.txt`。
- [ ] **AC2** `libra checkout --theirs -- f.txt` 把 stage 3 内容写入工作区 `f.txt`。
- [ ] **AC3** `libra checkout -2 -- f.txt` 把 stage 2 内容写入工作区 `f.txt`。
- [ ] **AC4** `libra checkout -3 -- f.txt` 把 stage 3 内容写入工作区 `f.txt`。
- [ ] **AC5** `libra checkout --theirs --no-overlay -- d.txt` 对无 stage 3 的 `d.txt` 从工作区删除该文件。
- [ ] **AC6** `libra checkout --ours HEAD -- f.txt` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC7** `libra checkout --theirs -b x` 以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F04：基提交 B（`f.txt`=`base\n`、`d.txt`=`d\n`）；分支 `ours` 把 `f.txt` 改为 `ours\n`、`d.txt` 改为 `d2\n`；分支 `theirs` 把 `f.txt` 改为 `theirs\n` 并删除 `d.txt`；在 `ours` 上 `libra merge theirs` 产生冲突。夹具前置断言：`libra ls-files --stage` 中 `f.txt` 有 stage 1/2/3、`d.txt` 有 stage 1/2 且无 stage 3，不满足即测试失败；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具（`git merge theirs`），快照按第 6 项）
- AC1 → V1：exit 0；工作区 `f.txt` 字节等于 Git 期望（`ours\n`）。
- AC2 → V1：exit 0；工作区 `f.txt` 字节等于 Git 期望（`theirs\n`）；对照：AC1 的 Git 期望不同。
- AC3 → V1：exit 0；工作区 `f.txt` 字节等于 Git 期望（`ours\n`；未注册 `-2` 时 clap 拒绝，可区分）。
- AC4 → V1：exit 0；工作区 `f.txt` 字节等于 Git 期望（`theirs\n`）。
- AC5 → V1：退出码、`S.wt` 与 `S.idx` 等于 Git 期望（`git checkout --theirs --no-overlay -- d.txt`：`d.txt` 不在工作区）。
- AC6 → V2：exit 非 0，stderr 含 `Error-Code: LBR-CLI-002`，`S` 不变。
- AC7 → V2：判定同 AC6（无 `refs/heads/x`）。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_ours_theirs_write_stage)'`（卡特有 `(new)`；覆盖 AC1–AC5；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_ours_theirs_rejections)'`（卡特有 `(new)`；覆盖 AC6–AC7；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::checkout::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`）；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-32, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`--ours`/`-2`、`--theirs`/`-3`、`--no-overlay` 删除无该端版本的路径、与 tree-ish/`-b`/`-B` 互斥）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--theirs`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件；单一公开选项面）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=checkout--theirs/--ours选边; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CSR-32,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-32; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-05: 修复 `checkout --no-overlay <tag>` 解析 tag 为源树（#704）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #704：修复 `checkout --no-overlay <tag> -- <pathspec>`（轻量与附注 tag）未把 tag 剥离到提交/树作为源树的问题，使 glob pathspec 命中子目录文件并按 no-overlay 删除源树缺失的已跟踪路径。
**Out of scope:** `--overlay` 默认与诊断（`CSR-03` 已交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `restore_checkout_paths` 把 `args.branch` 原样作为 `RestoreArgs.source`，附注 tag 未剥离 | `src/command/checkout.rs:706-757` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout --no-overlay lt -- '*file3'`（轻量 tag）把 `sub/file3` 恢复为 tag 所指内容。
- [ ] **AC2** `libra checkout --no-overlay at -- '*file3'`（附注 tag）把 `sub/file3` 恢复为 tag 所指内容。
- [ ] **AC3** `libra checkout --no-overlay at -- '*file3'` 从工作区与索引删除源树中不存在的已跟踪 `sub/extra-file3`。
- [ ] **AC4** 无 `--` 写法 `libra checkout --no-overlay at '*file3'` 结果与 AC2、AC3 相同。
- [ ] **AC5** `libra checkout --no-overlay nosuch -- '*file3'` 以 `LBR-CLI-003` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F05：C1 含 `file1`=`1\n`、`sub/file3`=`3a\n`，在 C1 建轻量 tag `lt` 与附注 tag `at`（`libra tag -a at -m at`）；C2=`HEAD` 把 `sub/file3` 改为 `3b\n` 并新增已跟踪 `sub/extra-file3`=`e\n`，工作区干净；每个 AC 在全新夹具上执行；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 0；`sub/file3` 字节等于 Git 期望（`3a\n`）。
- AC2 → V1：exit 0；`sub/file3` 字节等于 Git 期望（`3a\n`）；对照：附注 tag 对象名不等于 C1 提交对象名（夹具断言）。
- AC3 → V1：exit 0；`S.wt` 与 `S.idx` 等于 Git 期望（无 `sub/extra-file3`）。
- AC4 → V1：exit 0；`S.wt` 与 `S.idx` 等于 AC3 的 Git 期望。
- AC5 → V2：exit 非 0，stderr 含 `Error-Code: LBR-CLI-003`，`S` 不变。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::no_overlay_tag_source)'`（卡特有 `(new)`；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::no_overlay_invalid_tag_rejected)'`（卡特有 `(new)`；覆盖 AC5；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::restore_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::restore::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享恢复引擎 `src/command/restore.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-04, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`--no-overlay <tag>` 以 tag 所指提交为源树）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--no-overlay <tag>`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--no-overlay<tag>源树解析; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CSR-04,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-04; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-06: 修复 `checkout --no-progress .` 路径消歧与进度标志（#705）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #705：修复 `index_matches_path` 对 `.`（及子目录前缀）不匹配，导致 `libra checkout --no-progress .` 误入分支切换并被未跟踪文件拒绝的问题；并接受 `--progress`、`--no-progress`、`--quiet --progress` 组合按路径恢复。
**Out of scope:** 待恢复路径自身被未跟踪文件占用的真实碰撞（仍拒绝）；delayed-filter 进度输出（Libra 无 `filter.<driver>.process`，归入 `DEFER-CSR-02`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `index_matches_path` 仅按条目名与 `<token>/` 前缀匹配，`.` 不命中而落入分支切换 | `src/command/checkout.rs:664-704` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout --no-progress .` 的结果状态（工作区与索引）等于 Git 期望（恢复已删除的 `a.txt`，保留未跟踪 `u.txt` 与 `sub/b.txt`）。
- [ ] **AC2** 上述命令不进入分支切换（引用与 reflog 不变）。
- [ ] **AC3** `libra checkout --quiet --progress .` 的结果状态等于 Git 期望。
- [ ] **AC4** `libra checkout --progress .` 的结果状态等于 Git 期望。
- [ ] **AC5** 在子目录 `sub/` 中执行 `libra checkout --no-progress .` 只恢复 `sub/` 下被删除的已跟踪文件。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F06：已提交 `a.txt`=`A\n`、`sub/b.txt`=`B\n`；工作区删除 `a.txt`，新增未跟踪 `u.txt`=`U\n`；AC5 另删除 `sub/b.txt` 并以 `sub/` 为 `current_dir`；每个 AC 在全新夹具上执行；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 0；`S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git checkout --no-progress .`）。
- AC2 → V1：AC1 同一次调用前后 `S.ref` 与 `S.log` 逐字不变。
- AC3 → V1：exit 0；`S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git checkout --quiet --progress .`）。
- AC4 → V1：exit 0；`S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git checkout --progress .`）。
- AC5 → V1：exit 0；`S.wt` 与 `S.idx` 作为整体等于 Git 期望（在 `sub/` 中 `git checkout --no-progress .`：`sub/b.txt` 恢复、`a.txt` 仍缺失）。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::restore_deleted_with_untracked_present)'`（卡特有 `(new)`；覆盖 AC1–AC5；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::restore::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享恢复引擎 `src/command/restore.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-05, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`.` 作为 pathspec 的消歧与 `--[no-]progress`）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--no-progress`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=.路径消歧与进度选项; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=CSR-05,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-05; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-07: 拒绝 `checkout --detach` 与 `-b`/`-B`/`--orphan` 联用（#706）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #706：在 `checkout` 参数校验中拒绝 `--detach` 与 `-b`/`-B`/`--orphan` 联用（对齐 Git `'--detach' cannot be used with '-b/-B/--orphan'`），单独 `--detach` 行为不变。
**Out of scope:** 单独 `checkout --detach [<commit>]` 的既有行为（本卡只作回归）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CheckoutArgs.detach` 无互斥约束 | `src/command/checkout.rs:57-77` |
| Git：`force_detach && new_branch` 时 die | `git@6de20f6092:builtin/checkout.c:1712-1714` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout --detach -b foo` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC2** `libra checkout --detach -B foo` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC3** `libra checkout --detach --orphan foo` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC4** `libra checkout --detach topic` 后结果状态（`HEAD`、工作区与索引）等于 Git 期望（`HEAD` 分离于 C1）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F07：`main` 上提交 C1（`a.txt`=`1`）→ C2（新增 `b.txt`=`2`），分支 `topic` 指向 C1（与当前 `main` 不同，用以区分「原地 detach」），工作区干净；Git 期望按「验证执行协议」第 4 项，快照按第 6 项）
- AC1 → V1：exit 非 0，stderr 含 `Error-Code: LBR-CLI-002`，`S` 不变（无 `refs/heads/foo`）。
- AC2 → V1：判定同 AC1。
- AC3 → V1：判定同 AC1（`HEAD` 仍指向 `refs/heads/main`）。
- AC4 → V1：exit 0；`HEAD` 为分离状态且对象名等于 C1，`S.wt` 与 `S.idx` 等于 Git 期望（`git checkout --detach topic`：无 `b.txt`）。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::detach_incompatible_with_branch_create)'`（卡特有 `(new)`；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::checkout::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`）；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-06, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`--detach` 与 `-b`/`-B`/`--orphan` 互斥）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`-b/-B/--orphan`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--detach与-b/-B/--orphan互斥; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CSR-06,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-06; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-08: `libra checkout` 输出分支追踪 ahead/behind 状态（#707 部分 1/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #707 中追踪状态文本轴（`REL-CSR-03` 子卡 1/2，经 `CSR-31` 联合发布）：`checkout <branch>` 切换到配置了上游（`branch.<name>.remote`/`.merge`，含 `remote = .` 本地上游）的分支后，把与 `libra status` 同一口径的追踪段输出到 stdout（复用 `src/command/status/output.rs:690-765` 的渲染与 `src/command/status.rs:875` 的 ahead/behind 计数，GC-02；`status.rs:53` 的 `mod output` 与 `output.rs:667` 的 `render_upstream_human` 当前为私有，本卡在 `status.rs` 以 `pub(crate)` 导出该渲染入口），`Switched to branch …` 输出到 stderr（对齐 Git `t2020` 对 stdout 的 `test_cmp`）；同时把共享渲染器的落后与分叉文案对齐 pinned Git（落后行补 `, and can be fast-forwarded.`，分叉提示改为 `if you want to integrate the remote branch with yours`），`libra status` 随之一致。
**Out of scope:** `--track=inherit`（`CSR-10`）；`--detach`/`--quiet`/`--json` 抑制与上游缺失降级（`CSR-28`）。G-09 拆分登记：本卡拆出 `CSR-28`。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `render_checkout_output` 只打印 `Switched to branch …`，无追踪段 | `src/command/checkout.rs:1229-1286` |
| `status` 已有追踪段渲染与 ahead/behind 计数，但落后行缺 `, and can be fast-forwarded.`、分叉提示为 `to merge the remote branch into yours`，与 pinned Git 不同 | `src/command/status/output.rs:690-765`、`src/command/status.rs:875`、`git@6de20f6092:remote.c:2397-2441` |
**Acceptance criteria:**
- [ ] **AC1** 上游为远程追踪分支且持平时，`libra checkout topic` 的 stdout 等于 Git 期望（`Your branch is up to date with 'origin/main'.`）。
- [ ] **AC2** 上游为本地分支（`remote = .`）且持平时，`libra checkout lt` 的 stdout 等于 Git 期望（`Your branch is up to date with 'base0'.`）。
- [ ] **AC3** 领先上游时 stdout 等于 Git 期望（`ahead of 'origin/main' by 1 commit` 段）。
- [ ] **AC4** 落后上游时 stdout 等于 Git 期望（`behind` 段与 `libra pull` 提示行）。
- [ ] **AC5** 与上游分叉时 stdout 等于 Git 期望（`have diverged` 段与 `libra pull` 提示行）。
- [ ] **AC6** `Switched to branch 'topic'` 只出现在 stderr。
- [ ] **AC7** 落后场景下 `libra status` 输出的追踪段等于 Git 期望（`git status` 同段）。
- [ ] **AC8** 分叉场景下 `libra status` 输出的追踪段等于 Git 期望。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-31` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F08：作为远端的 Libra 仓库 O 的 `main` 有提交 C0（树：`a.txt`=`a\n`）；本地仓库 R 先 `libra init`、提交 Cr（树：`r.txt`=`r\n`）作为 R 的 `main`，再 `libra remote add origin <O 路径>`、`libra fetch origin`；R 中以 `libra branch topic origin/main` 建 `topic` 并 `libra config set branch.topic.remote origin`、`libra config set branch.topic.merge refs/heads/main`；本地上游场景另以 `libra branch base0 origin/main`、`libra branch lt origin/main` 建两分支，并设 `branch.lt.remote .`、`branch.lt.merge refs/heads/base0`（`lt` 与 `base0` 同指 C0）；ahead：`libra switch topic`，写 `t.txt`=`t\n`、`libra add t.txt`、`libra commit -m t1`，再 `libra switch main`；behind：在 O 中写 `o.txt`=`o\n` 并提交，R 再 `libra fetch origin`；diverged：两者都做（`t.txt` 与 `o.txt` 互不冲突）；每个场景最后都从 `main` 执行 `libra checkout topic`（或 `lt`）；Git 期望按「验证执行协议」第 4 项取自按同一步骤构造的 Git 夹具的 `git checkout topic` 输出（提示中的 `git <子命令>` 按第 4(a) 项替换为 `libra <子命令>`），快照按第 6 项）
- AC1 → V1：exit 0；stdout 逐字等于 Git 期望。
- AC2 → V1：exit 0；stdout 逐字等于 Git 期望；对照：AC1 的 Git 期望上游名不同。
- AC3 → V1：exit 0；stdout 逐字等于 Git 期望。
- AC4 → V1：exit 0；stdout 逐字等于 Git 期望。
- AC5 → V1：exit 0；stdout 逐字等于 Git 期望。
- AC6 → V1：AC1 那次调用的 stderr 含 `Switched to branch 'topic'`，stdout 不含 `Switched to branch`。
- AC7 → V1：切到 `topic` 后运行 `libra status`；exit 0；stdout 中自以 `Your branch` 开头的行起、至其后第一个空行（不含）止的整段（含计数行与缩进提示行）逐字等于 Git 期望（`git status` 同段）。
- AC8 → V1：判定同 AC7（分叉场景）。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_prints_tracking_status)'`（卡特有 `(new)`；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::status_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::status::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-31` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-07, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/status.rs`, `src/command/status/output.rs`, `tests/command/checkout_test.rs`, `tests/command/status_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（切换后输出追踪段、`Switched to branch` 位于 stderr）、`docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`（落后/分叉追踪文案对齐 Git）、网站 `checkout.en.md` 与 `status.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout`、`status` 两行原地更新）。文档关键字（EN/zh 命令页）：`can be fast-forwarded`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（2 个落点，3 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-03 (1/2)
**C/D coverage from:** CSR-31
**Granularity:** `type=implementation; axis=checkout追踪状态文本; recovery=revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=CSR-07,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-07; release=family child; split-from=N/A; exception=N/A`

---

### Task CSR-28: `checkout` 追踪状态 `--detach`/`--quiet`/`--json` 与缺引用降级（#707 部分 2/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #707 中抑制与降级轴（G-09：自 `CSR-08` 拆出；`REL-CSR-03` 子卡 2/2）：`--detach` 与 `--quiet` 不输出追踪段，`--json` 输出保持单个合法 JSON 对象，上游引用缺失时不中断切换。
**Out of scope:** 追踪段文本（`CSR-08` 已交付）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `render_checkout_output` 需区分 `--detach`/`--quiet`/`--json` 与上游引用缺失 | `src/command/checkout.rs:1229-1286` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout --detach topic`（落后场景）的 stdout 不含以 `Your branch` 开头的行。
- [ ] **AC2** `libra checkout --quiet topic`（落后场景）的 stdout 不含以 `Your branch` 开头的行。
- [ ] **AC3** `libra --json checkout topic`（落后场景）的 stdout 是单个合法 JSON 对象。
- [ ] **AC4** 上游引用缺失（`branch.topic.merge` 为 `refs/heads/gone`）时 `libra checkout topic` 仍切换到 `topic`。
- [ ] **AC5** AC1 的 `--detach` 调用后结果状态（`HEAD`、工作区与索引）等于 Git 期望（`HEAD` 分离于 `topic` 的提交）。
- [ ] **AC6** AC2 的 `--quiet` 调用后结果状态等于 Git 期望（`HEAD` 指向 `topic`）。
- [ ] **AC7** AC3 的 `--json` 调用后结果状态等于 Git 期望（`HEAD` 指向 `topic`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-31` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F08：作为远端的 Libra 仓库 O 的 `main` 有提交 C0（树：`a.txt`=`a\n`）；本地仓库 R 先 `libra init`、提交 Cr（树：`r.txt`=`r\n`）作为 R 的 `main`，再 `libra remote add origin <O 路径>`、`libra fetch origin`；R 中以 `libra branch topic origin/main` 建 `topic` 并 `libra config set branch.topic.remote origin`、`libra config set branch.topic.merge refs/heads/main`；本地上游场景另以 `libra branch base0 origin/main`、`libra branch lt origin/main` 建两分支，并设 `branch.lt.remote .`、`branch.lt.merge refs/heads/base0`（`lt` 与 `base0` 同指 C0）；ahead：`libra switch topic`，写 `t.txt`=`t\n`、`libra add t.txt`、`libra commit -m t1`，再 `libra switch main`；behind：在 O 中写 `o.txt`=`o\n` 并提交，R 再 `libra fetch origin`；diverged：两者都做（`t.txt` 与 `o.txt` 互不冲突）；每个场景最后都从 `main` 执行 `libra checkout topic`（或 `lt`）；Git 期望按「验证执行协议」第 4 项取自按同一步骤构造的 Git 夹具的 `git checkout topic` 输出（提示中的 `git <子命令>` 按第 4(a) 项替换为 `libra <子命令>`），快照按第 6 项；R 的 `main`（Cr，只含 `r.txt`）与 `topic`（C0 系，含 `a.txt`）树不同；AC1–AC3 使用 behind 场景，AC4 另把 `branch.topic.merge` 设为 `refs/heads/gone`（O 无该分支））
- AC1 → V1：exit 0；stdout 中不存在以 `Your branch` 开头的行；对照：同夹具不带 `--detach` 时 Git 期望含该行。
- AC2 → V1：exit 0；stdout 中不存在以 `Your branch` 开头的行（Git 期望 stdout 为空）。
- AC3 → V1：exit 0；stdout 去掉末尾换行后经 `serde_json::from_str::<serde_json::Value>` 解析为对象。
- AC4 → V1：exit 0；`HEAD` 指向 `refs/heads/topic`。
- AC5 → V1：AC1 同一次调用后 `HEAD` 分离且对象名等于 `topic` 的提交，`S.wt` 与 `S.idx` 等于 Git 期望。
- AC6 → V1：AC2 同一次调用后 `HEAD` 指向 `refs/heads/topic`，`S.wt` 与 `S.idx` 等于 Git 期望。
- AC7 → V1：AC3 同一次调用后 `HEAD` 指向 `refs/heads/topic`，`S.wt` 与 `S.idx` 等于 Git 期望。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_tracking_status_suppression_and_json)'`（卡特有 `(new)`；覆盖 AC1–AC7；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-31` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-08, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`--detach`/`--quiet` 不输出追踪段、`--json` 形态、上游缺失时照常切换）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--quiet`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-03 (2/2)
**C/D coverage from:** CSR-31
**Granularity:** `type=implementation; axis=checkout追踪状态抑制与降级; recovery=revert本地提交; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CSR-08,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-08; release=family child; split-from=CSR-08; exception=N/A`

---

### Task CSR-09: 支持 `checkout --pathspec-from-file=<file>` 读取与全部互斥校验（#708 部分 1/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #708 中 `--pathspec-from-file=<file>` 读取、缺失诊断，以及与 `--detach`、位置 pathspec、`-p`/`--patch`、`-b`、`-B`、`--orphan` 的互斥（同一恢复轴；原 `CSR-35` 已合回本卡）；读取复用 `restore.rs:383` 的 `read_restore_pathspec_file`（GC-02）。`REL-CSR-05` 子卡 1/3，经 `CSR-34` 联合发布。
**Out of scope:** `--pathspec-from-file=-`（G-09：拆出 `CSR-33`）；`--pathspec-file-nul`（`CSR-24`）；`-B`/`--orphan` 与 `--pathspec-from-file` 联用由与 `-b` 相同的路径模式守卫（`checkout.rs:706-716` 的 `InvalidPathMode`，`LBR-CLI-002`）拒绝，本卡以 `-b` 覆盖；`<p>` 不存在时由共享读取器以 `LBR-IO-001` 报错（`restore.rs:383-394,157`），非 #708 验收项；路径文件中 C 风格引号路径的解析不在 #708 验收内，登记 `DEFER-CSR-04`。G-09 拆分登记：本卡拆出 `CSR-33`、`CSR-24`。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CheckoutArgs` 无 `--pathspec-from-file`；`restore` 已有读取器，缺失文件映射 `LBR-IO-001` | `src/command/checkout.rs:53-120`、`src/command/restore.rs:383-394,157` |
| Git：`--pathspec-from-file` 与 pathspec 参数、`--detach`、`--patch` 互斥；与 `-b`/`-B`/`--orphan` 联用落入「不能同时更新路径并切换分支」 | `git@6de20f6092:builtin/checkout.c:2058-2069,550-552` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout --pathspec-from-file=<p>` 按行读取 `<p>` 并恢复 `a.txt` 与 `b.txt`，保留 `c.txt` 的修改。
- [ ] **AC2** 与 `--detach` 联用时以 `LBR-CLI-002` 拒绝。
- [ ] **AC3** 上述拒绝的 stderr 含 `options '--pathspec-from-file' and '--detach' cannot be used together`（#708 验收 2）。
- [ ] **AC4** 与 `-p` 联用时以 `LBR-CLI-002` 拒绝。
- [ ] **AC5** 上述拒绝的 stderr 含 `options '--pathspec-from-file' and '--patch' cannot be used together`（#708 验收 3）。
- [ ] **AC6** 与位置 pathspec（`-- c.txt`）联用时以 `LBR-CLI-002` 拒绝。
- [ ] **AC7** 上述拒绝的 stderr 含 `'--pathspec-from-file' and pathspec arguments cannot be used together`（#708 验收 4）。
- [ ] **AC8** 与 `-b x` 联用时以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-34` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F09：已提交 `a.txt`=`A\n`、`b.txt`=`B\n`、`c.txt`=`C\n`；工作区删除 `a.txt` 与 `b.txt`，把 `c.txt` 改为 `C2\n`；路径文件放在仓库外测试临时目录：`<p>`=`a.txt\nb.txt\n`；Git 期望按「验证执行协议」第 4 项，快照按第 6 项）
- AC1 → V1：exit 0；`S.wt` 与 `S.idx` 作为整体等于 Git 期望（`a.txt`、`b.txt` 恢复，`c.txt` 仍为 `C2`）。
- AC2 → V2：exit 非 0，stderr 含 `Error-Code: LBR-CLI-002`，`S` 不变。
- AC3 → V2：AC2 同一次调用的 stderr 含该文案。
- AC4 → V2：判定同 AC2。
- AC5 → V2：AC4 同一次调用的 stderr 含该文案。
- AC6 → V2：判定同 AC2。
- AC7 → V2：AC6 同一次调用的 stderr 含该文案。
- AC8 → V2：判定同 AC2（无 `refs/heads/x`）。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::pathspec_from_file_reads_lines)'`（卡特有 `(new)`；覆盖 AC1；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::pathspec_from_file_incompatible_options_rejected)'`（卡特有 `(new)`；覆盖 AC2–AC8；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::restore::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-34` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-31, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`--pathspec-from-file=<file>` 与其互斥）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--pathspec-from-file`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-05 (1/3)
**C/D coverage from:** CSR-34
**Granularity:** `type=implementation; axis=--pathspec-from-file=<file>读取与全部互斥; recovery=revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CSR-31,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-31; release=family child; split-from=N/A; exception=N/A`

---

### Task CSR-35: 已合并回 CSR-09（废弃，不得开工）

**Status:** superseded by `CSR-09`（R22，G-02）。本编号不再拥有写集、验收标准或发布边界，不计入活动卡，也不进入 `REL-CSR-05`。

---

### Task CSR-33: 支持 `checkout --pathspec-from-file=-` 标准输入读取（#708 部分 2/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #708 中 `--pathspec-from-file=-` 标准输入读取轴（G-09：自 `CSR-09` 拆出；`REL-CSR-05` 子卡 2/3）：把 `-` 交给 `restore.rs:383` 现有读取器（其已处理 `path == "-"`）。实现约束（GC-02，由 ER-05 review 核对，不作卡特有 AC）：`checkout` 不新增第二套 pathspec 文件或标准输入读取实现。
**Out of scope:** `--pathspec-from-file=<file>` 与互斥（`CSR-09`）；`--pathspec-file-nul`（`CSR-24`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `read_restore_pathspec_file` 已处理 `-` 读取标准输入；`checkout` 尚未接入 | `src/command/restore.rs:383-390`、`src/command/checkout.rs:53-120` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout --pathspec-from-file=-` 从标准输入按行读取并恢复 `a.txt` 与 `b.txt`。
- [ ] **AC2** 标准输入为空时 `libra checkout --pathspec-from-file=-` 退出 0 且不改动仓库。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-34` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F09：已提交 `a.txt`=`A\n`、`b.txt`=`B\n`、`c.txt`=`C\n`；工作区删除 `a.txt` 与 `b.txt`，把 `c.txt` 改为 `C2\n`；路径文件放在仓库外测试临时目录；AC1 stdin 为 `a.txt\nb.txt\n`，AC2 stdin 为空；Git 期望按「验证执行协议」第 4 项，快照按第 6 项）
- AC1 → V1：exit 0；`S.wt` 与 `S.idx` 作为整体等于 Git 期望（`a.txt`、`b.txt` 恢复，`c.txt` 仍为 `C2`）。
- AC2 → V1：exit 等于 Git 期望（0）；`S` 与运行前逐字相等。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::pathspec_from_stdin)'`（卡特有 `(new)`；覆盖 AC1–AC2；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::restore::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-34` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-09, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`--pathspec-from-file=-` 读标准输入、空输入为无操作）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--pathspec-from-file=-`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-05 (2/3)
**C/D coverage from:** CSR-34
**Granularity:** `type=implementation; axis=--pathspec-from-file=-标准输入; recovery=revert本地提交; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=CSR-09,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-09; release=family child; split-from=CSR-09; exception=N/A`

---

### Task CSR-24: 支持 `checkout --pathspec-file-nul` NUL 切分与单独报错（#708 部分 3/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #708 中 `--pathspec-file-nul` 轴（`REL-CSR-05` 子卡 3/3）：搭配 `--pathspec-from-file=<file>` 或 `-` 时按 NUL 切分（复用 `read_restore_pathspec_file` 的 `nul` 参数），单独使用时按 Git 拒绝。
**Out of scope:** `--pathspec-from-file` 读取与互斥（`CSR-09`）；标准输入（`CSR-33`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CheckoutArgs` 无 `--pathspec-file-nul`；读取器已支持 NUL 切分 | `src/command/checkout.rs:53-120`、`src/command/restore.rs:383,396-406` |
| Git：`--pathspec-file-nul` 未带 `--pathspec-from-file` 时 die | `git@6de20f6092:builtin/checkout.c:2071-2072` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout --pathspec-from-file=<n> --pathspec-file-nul` 按 NUL 切分并恢复 `a.txt` 与 `b.txt`。
- [ ] **AC2** `libra checkout --pathspec-from-file=- --pathspec-file-nul` 按 NUL 切分标准输入并恢复 `a.txt` 与 `b.txt`。
- [ ] **AC3** `libra checkout --pathspec-file-nul`（无 `--pathspec-from-file`）以 `LBR-CLI-002` 拒绝。
- [ ] **AC4** 上述拒绝的 stderr 含 `the option '--pathspec-file-nul' requires '--pathspec-from-file'`（#708 验收 5）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-34` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F09：已提交 `a.txt`=`A\n`、`b.txt`=`B\n`、`c.txt`=`C\n`；工作区删除 `a.txt` 与 `b.txt`，把 `c.txt` 改为 `C2\n`；路径文件放在仓库外测试临时目录；`<n>` 与 AC2 的 stdin 内容均为 `a.txt\0b.txt\0`（无换行；若按换行切分则得到单个无效路径）；Git 期望按「验证执行协议」第 4 项，快照按第 6 项）
- AC1 → V1：exit 0；`S.wt` 与 `S.idx` 等于 Git 期望。
- AC2 → V1：exit 0；`S.wt` 与 `S.idx` 等于 Git 期望。
- AC3 → V1：exit 非 0，stderr 含 `Error-Code: LBR-CLI-002`，`S` 不变。
- AC4 → V1：AC3 同一次调用的 stderr 含该文案。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::pathspec_file_nul)'`（卡特有 `(new)`；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::restore::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-34` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-33, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`--pathspec-file-nul` 与其依赖）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--pathspec-file-nul`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-05 (3/3)
**C/D coverage from:** CSR-34
**Granularity:** `type=implementation; axis=--pathspec-file-nul切分与报错; recovery=revert本地提交; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=CSR-33,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-33; release=family child; split-from=CSR-09; exception=N/A`

---

### Task CSR-34: `REL-CSR-05` (`--pathspec-from-file` 与 `--pathspec-file-nul`) 家族发布点

**Task type:** release
**Lifecycle / Acceptance:** pending / 空
**Description:** 聚合 `CSR-09`, `CSR-33`, `CSR-24`（`REL-CSR-05`），在全部子卡本地签名提交并通过本地 A/B、ER-05 与 fmt/clippy 后，统一执行 C 组（`patch + 1`、版本面三处同步、工具链刷新 `Cargo.lock`、T-2 全量、签名提交、PR 与 squash merge、`gh release create`）、D 组与家族网站第 5–7 步，作为该家族唯一发布点（G-08）。
**Out of scope:** 不引入新产品代码或文档内容改动。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CSR-09`, `CSR-33`, `CSR-24` 同属一个公开行为面，须同版发布 | 「发布分组与并发窗口」`REL-CSR-05` |
**Acceptance criteria:**
- [ ] **AC1** `CSR-09` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC2** `CSR-33` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC3** `CSR-24` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC4** 聚合守卫：家族全部卡特有用例（下方「聚合守卫」所列）在已 bump 的发布树上逐条按「验证执行协议」第 1–3 项通过。
- [ ] **AC5** `gh release view v<version>` 的 release note 正文逐条列出 `CSR-09`、`CSR-33` 与 `CSR-24` 的用户可见变化。
- [ ] **AC6** 每张子卡在「文档与网站验收命令」第 1 步登记的每个仓库文档改动后 SHA-256，等于该子卡本地签名提交中同一文件的 SHA-256。
- [ ] **AC7** 每个被家族子卡修改的仓库文档在发布树中的 SHA-256，等于家族顺序中最后修改它的子卡的登记值。
- [ ] **AC8** 家族网站页 `checkout.en.md` 按「文档与网站验收命令」第 2–7 步完成（第 3 步以本卡合入 `main` 后的家族最终 EN 文档同步，第 8 项家族口径）。
（ER-04 的 fmt / clippy / T-2 全量与 ER-08 版本面门按 G-03 不计入本卡特有 AC/VER 分子。）
**AC → 断言映射:**（非测试型判据按「验证执行协议」第 9 项；`<REL>` 为本卡 C 组第⑦步签名提交 SHA，`<子卡 提交>` 为各子卡本地签名提交 SHA（登记于子卡记录））
- AC1 → V1：`libra merge-base --is-ancestor <CSR-09 提交> <REL>` exit 0。
- AC2 → V2：`libra merge-base --is-ancestor <CSR-33 提交> <REL>` exit 0。
- AC3 → V3：`libra merge-base --is-ancestor <CSR-24 提交> <REL>` exit 0。
- AC4 → 聚合守卫：所列每条命令按协议第 1–3 项判定全部通过（ER-04 B 组 `release` 类型门），逐条记录 exit code 与计数。
- AC5 → V4：`gh release view v<version> --json body --jq .body` exit 0，正文对 `CSR-09`、`CSR-33` 与 `CSR-24` 各有一条变化说明（逐条以卡 ID 标注）。
- AC6 → V5：对每张子卡登记的每个文档执行 `libra show <该子卡提交>:<路径> | shasum -a 256`，逐一等于其登记值；任一不等即失败。
- AC7 → V6：对每个文档取最后修改它的子卡登记值，与 `libra show <REL>:<路径> | shasum -a 256` 比较相等；任一不等即失败。
- AC8 → V7：第 2–7 步逐步记录命令、退出码与 SHA-256；任一步 STOP 即不成立。
**Verification:**
- [ ] V1 `libra merge-base --is-ancestor <CSR-09 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V2 `libra merge-base --is-ancestor <CSR-33 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V3 `libra merge-base --is-ancestor <CSR-24 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V4 `gh release view v<version> --json body --jq .body`（非测试型，按协议第 9 项；exit 0 且逐卡说明齐全为通过）
- [ ] V5 `libra show <子卡提交>:<路径> | shasum -a 256`（逐子卡逐文件；非测试型，按协议第 9 项）
- [ ] V6 `libra show <REL>:<路径> | shasum -a 256`（逐文件；非测试型，按协议第 9 项）
- [ ] V7 「文档与网站验收命令」第 2–7 步（非测试型，按协议第 9 项；各步判据见该节）
**聚合守卫（ER-04 B 组 `release` 类型门，不计入 VER）：** `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::pathspec_from_file_reads_lines)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::pathspec_from_file_incompatible_options_rejected)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::pathspec_from_stdin)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::pathspec_file_nul)'`
**Full-suite trigger:** T-2：`family release point`（C 组第④步在 bump 后的精确提交树上跑 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 与 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，记录提交 SHA 与 `HEAD^{tree}`；合入与发布前核对发布树与已测树一致，变化即重跑）
**Dependencies:** CSR-24, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `docs/development/plan/plan-20261117-checkout-switch-restore.md`（发布证据登记）, `tests/INDEX.md`（仅需同步时）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`（第 6 步提交）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 版本面三处（`Cargo.toml`、`install.sh`、`install.ps1`）与工具链刷新的 `Cargo.lock`（属 `Release write set`）
**Docs and compatibility impact:** 不新增文档内容：汇总子卡文档交付证据（AC6、AC7），以家族最终 EN 文档完成网站 `checkout.en.md` 第 2–7 步（AC8）。
**Rollback mode:** immutable-release
**Estimated scope:** S（0 个生产文件，仅家族发布聚合）
**Version increment:** patch
**Release boundary:** family release point of REL-CSR-05
**C/D coverage from:** self
**Granularity:** `type=release; axis=REL-CSR-05家族发布点; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/12; VER=7/12; landing=0; prod-files=0; scope=S; deps=CSR-24,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-24; release=family release point; split-from=N/A; exception=N/A`

---

### Task CSR-10: 支持 `checkout --track=inherit|direct` 与 `branch.autoSetupMerge=inherit`（#709）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #709（`REL-CSR-TRACK` 子卡 1/2，经 `CSR-10R` 联合发布）：把 `CheckoutArgs.track` 由 `bool` 升级为可带值的追踪模式（复用 `branch::BranchTrackMode`，GC-02），支持 `--track=direct`、`--track=inherit` 与 `--no-track`，未显式指定时按 `branch.autoSetupMerge`（`true`/`always`/`inherit`/`false`）决定是否设置上游。
**Out of scope:** 裸 `--track`（不带值）的 direct 兼容（`CSR-10a`）；`branch` 命令自身的 `--track`。G-09 拆分登记：本卡拆出 `CSR-10a`。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CheckoutArgs.track` 为 `bool`；`maybe_set_startpoint_upstream` 不读 `branch.autoSetupMerge=inherit` | `src/command/checkout.rs:84-85,899-914` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout -b n1 --track=inherit base` 复制 `base` 的上游配置。
- [ ] **AC2** `libra checkout -b n2 --track=direct base` 把 `base` 设为上游（`remote = .`）。
- [ ] **AC3** `branch.autoSetupMerge=true` 时 `libra checkout -b n3 origin/main` 设置上游为 `origin/main`。
- [ ] **AC4** `branch.autoSetupMerge=always` 时 `libra checkout -b n4 lb` 设置上游为本地 `lb`。
- [ ] **AC5** `branch.autoSetupMerge=inherit` 时 `libra checkout -b n5 base` 复制 `base` 的上游配置。
- [ ] **AC6** `branch.autoSetupMerge=false` 时 `libra checkout -b n6 origin/main` 不设置上游。
- [ ] **AC7** `branch.autoSetupMerge=always` 时 `libra checkout --no-track -b n7 lb` 不设置上游。
- [ ] **AC8** `libra checkout -b n8 --track=bad base` 以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-10R` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F10：作为远端的 Libra 仓库 O 的 `main` 有提交 C0（树：`a.txt`=`a\n`）；本地仓库 R 先 `libra init` 并提交 Cr（树：`r.txt`=`r\n`）作为 `main`，再 `libra remote add origin <O 路径>`、`libra fetch origin`；以 `libra branch base origin/main` 建 `base`（C0）并设 `branch.base.remote origin`、`branch.base.merge refs/heads/main`；以 `libra branch lb base` 建 `lb`（C0，本地起点在默认 `branch.autoSetupMerge` 下无上游，夹具断言 `S.cfg` 无 `branch.lb.*`）；每个 AC 新建的分支名互不相同；「上游配置」= `S.cfg` 中 `branch.<新分支>.remote` 与 `branch.<新分支>.merge` 两行（Git 无该键时 Libra 亦须无该行）；Git 期望按「验证执行协议」第 4 项取自按同一步骤构造的 Git 夹具（`git config branch.<新分支>.remote/.merge`），快照按第 6 项）
- AC1 → V1：exit 0；上游配置等于 Git 期望（`origin` 与 `refs/heads/main`）；对照：AC2 的 Git 期望不同。
- AC2 → V1：exit 0；上游配置等于 Git 期望（`.` 与 `refs/heads/base`）。
- AC3 → V1：exit 0；上游配置等于 Git 期望（`origin` 与 `refs/heads/main`）。
- AC4 → V1：exit 0；上游配置等于 Git 期望（`.` 与 `refs/heads/lb`）。
- AC5 → V1：exit 0；上游配置等于 Git 期望（`origin` 与 `refs/heads/main`）。
- AC6 → V1：exit 0；上游配置等于 Git 期望（两键均不存在）。
- AC7 → V1：exit 0；上游配置等于 Git 期望（两键均不存在）。
- AC8 → V1：exit 非 0，stderr 含 `Error-Code: LBR-CLI-002`，`S` 不变。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::track_inherit_and_auto_setup_merge)'`（卡特有 `(new)`；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::branch_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-10R` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-34, DEP-CSR-02, DEP-CSR-06, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/branch.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`--track=direct|inherit`、`--no-track` 与 `branch.autoSetupMerge`）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--track=inherit`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-TRACK (1/2)
**C/D coverage from:** CSR-10R
**Granularity:** `type=implementation; axis=--track=inherit/direct与autoSetupMerge; recovery=revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=CSR-34,DEP-CSR-02,DEP-CSR-06,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-34; release=family child; split-from=N/A; exception=N/A`

---

### Task CSR-10a: 保留裸 `checkout --track` 的 direct 兼容行为

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** G-09：自 `CSR-10` 拆出的向后兼容轴（`REL-CSR-TRACK` 子卡 2/2）：`CheckoutArgs.track` 改为可带值后，不带值的 `--track` 仍按 Git 等同 `--track=direct`，`checkout --track <remote>/<branch>` 仍自动推导本地分支名。
**Out of scope:** `--track=inherit/direct` 与 `branch.autoSetupMerge`（`CSR-10`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| 当前裸 `--track` 为布尔开关；升级为可带值后须保持不带值写法可解析 | `src/command/checkout.rs:84-85` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout -b n9 --track base` 设置的上游配置等于 Git 期望（与 `--track=direct` 相同）。
- [ ] **AC2** `libra checkout --track origin/main`（无 `-b`，本地无 `main`）新建由远程追踪名推导的本地 `main` 并使 `HEAD` 指向它。
- [ ] **AC3** 上述新建的 `main` 的上游配置等于 Git 期望。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-10R` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F10：作为远端的 Libra 仓库 O 的 `main` 有提交 C0（树：`a.txt`=`a\n`）；本地仓库 R 先 `libra init` 并提交 Cr（树：`r.txt`=`r\n`）作为 `main`，再 `libra remote add origin <O 路径>`、`libra fetch origin`；以 `libra branch base origin/main` 建 `base`（C0）并设 `branch.base.remote origin`、`branch.base.merge refs/heads/main`；以 `libra branch lb base` 建 `lb`（C0，本地起点在默认 `branch.autoSetupMerge` 下无上游，夹具断言 `S.cfg` 无 `branch.lb.*`）；每个 AC 新建的分支名互不相同；「上游配置」= `S.cfg` 中 `branch.<新分支>.remote` 与 `branch.<新分支>.merge` 两行（Git 无该键时 Libra 亦须无该行）；Git 期望按「验证执行协议」第 4 项取自按同一步骤构造的 Git 夹具（`git config branch.<新分支>.remote/.merge`），快照按第 6 项；AC2 先以 `libra branch -m main trunk` 把 R 的本地 `main` 改名为 `trunk`（`HEAD` 随之指向 `trunk`），使本地不存在 `main`）
- AC1 → V1：exit 0；上游配置等于 Git 期望（`.` 与 `refs/heads/base`）。
- AC2 → V1：exit 0；本地分支 `main` 存在且指向 C0，`HEAD` 指向 `refs/heads/main`。
- AC3 → V1：AC2 同一次调用后上游配置等于 Git 期望（`origin` 与 `refs/heads/main`）。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::bare_track_matches_direct)'`（卡特有 `(new)`；覆盖 AC1–AC3；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-10R` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-10, DEP-CSR-02, DEP-CSR-06, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（裸 `--track` 等同 `--track=direct`）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--track=direct`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-TRACK (2/2)
**C/D coverage from:** CSR-10R
**Granularity:** `type=implementation; axis=裸--track的direct兼容; recovery=revert本地提交; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CSR-10,DEP-CSR-02,DEP-CSR-06,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-10; release=family child; split-from=CSR-10; exception=N/A`

---

### Task CSR-10R: `REL-CSR-TRACK` 追踪继承与裸 track 联合发布点

**Task type:** release
**Lifecycle / Acceptance:** pending / 空
**Description:** 聚合 `CSR-10`, `CSR-10a`（`REL-CSR-TRACK`），在全部子卡本地签名提交并通过本地 A/B、ER-05 与 fmt/clippy 后，统一执行 C 组（`patch + 1`、版本面三处同步、工具链刷新 `Cargo.lock`、T-2 全量、签名提交、PR 与 squash merge、`gh release create`）、D 组与家族网站第 5–7 步，作为该家族唯一发布点（G-08）。
**Out of scope:** 不引入新产品代码或文档内容改动。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CSR-10`, `CSR-10a` 同属一个公开行为面，须同版发布 | 「发布分组与并发窗口」`REL-CSR-TRACK` |
**Acceptance criteria:**
- [ ] **AC1** `CSR-10` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC2** `CSR-10a` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC3** 聚合守卫：家族全部卡特有用例（下方「聚合守卫」所列）在已 bump 的发布树上逐条按「验证执行协议」第 1–3 项通过。
- [ ] **AC4** `gh release view v<version>` 的 release note 正文逐条列出 `CSR-10` 与 `CSR-10a` 的用户可见变化。
- [ ] **AC5** 每张子卡在「文档与网站验收命令」第 1 步登记的每个仓库文档改动后 SHA-256，等于该子卡本地签名提交中同一文件的 SHA-256。
- [ ] **AC6** 每个被家族子卡修改的仓库文档在发布树中的 SHA-256，等于家族顺序中最后修改它的子卡的登记值。
- [ ] **AC7** 家族网站页 `checkout.en.md` 按「文档与网站验收命令」第 2–7 步完成（第 3 步以本卡合入 `main` 后的家族最终 EN 文档同步，第 8 项家族口径）。
（ER-04 的 fmt / clippy / T-2 全量与 ER-08 版本面门按 G-03 不计入本卡特有 AC/VER 分子。）
**AC → 断言映射:**（非测试型判据按「验证执行协议」第 9 项；`<REL>` 为本卡 C 组第⑦步签名提交 SHA，`<子卡 提交>` 为各子卡本地签名提交 SHA（登记于子卡记录））
- AC1 → V1：`libra merge-base --is-ancestor <CSR-10 提交> <REL>` exit 0。
- AC2 → V2：`libra merge-base --is-ancestor <CSR-10a 提交> <REL>` exit 0。
- AC3 → 聚合守卫：所列每条命令按协议第 1–3 项判定全部通过（ER-04 B 组 `release` 类型门），逐条记录 exit code 与计数。
- AC4 → V3：`gh release view v<version> --json body --jq .body` exit 0，正文对 `CSR-10` 与 `CSR-10a` 各有一条变化说明（逐条以卡 ID 标注）。
- AC5 → V4：对每张子卡登记的每个文档执行 `libra show <该子卡提交>:<路径> | shasum -a 256`，逐一等于其登记值；任一不等即失败。
- AC6 → V5：对每个文档取最后修改它的子卡登记值，与 `libra show <REL>:<路径> | shasum -a 256` 比较相等；任一不等即失败。
- AC7 → V6：第 2–7 步逐步记录命令、退出码与 SHA-256；任一步 STOP 即不成立。
**Verification:**
- [ ] V1 `libra merge-base --is-ancestor <CSR-10 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V2 `libra merge-base --is-ancestor <CSR-10a 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V3 `gh release view v<version> --json body --jq .body`（非测试型，按协议第 9 项；exit 0 且逐卡说明齐全为通过）
- [ ] V4 `libra show <子卡提交>:<路径> | shasum -a 256`（逐子卡逐文件；非测试型，按协议第 9 项）
- [ ] V5 `libra show <REL>:<路径> | shasum -a 256`（逐文件；非测试型，按协议第 9 项）
- [ ] V6 「文档与网站验收命令」第 2–7 步（非测试型，按协议第 9 项；各步判据见该节）
**聚合守卫（ER-04 B 组 `release` 类型门，不计入 VER）：** `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::track_inherit_and_auto_setup_merge)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::bare_track_matches_direct)'`
**Full-suite trigger:** T-2：`family release point`（C 组第④步在 bump 后的精确提交树上跑 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 与 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，记录提交 SHA 与 `HEAD^{tree}`；合入与发布前核对发布树与已测树一致，变化即重跑）
**Dependencies:** CSR-10a, DEP-CSR-02, DEP-CSR-06, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `docs/development/plan/plan-20261117-checkout-switch-restore.md`（发布证据登记）, `tests/INDEX.md`（仅需同步时）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`（第 6 步提交）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 版本面三处（`Cargo.toml`、`install.sh`、`install.ps1`）与工具链刷新的 `Cargo.lock`（属 `Release write set`）
**Docs and compatibility impact:** 不新增文档内容：汇总子卡文档交付证据（AC5、AC6），以家族最终 EN 文档完成网站 `checkout.en.md` 第 2–7 步（AC7）。
**Rollback mode:** immutable-release
**Estimated scope:** S（0 个生产文件，仅家族发布聚合）
**Version increment:** patch
**Release boundary:** family release point of REL-CSR-TRACK
**C/D coverage from:** self
**Granularity:** `type=release; axis=REL-CSR-TRACK家族发布点; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/12; VER=6/12; landing=0; prod-files=0; scope=S; deps=CSR-10a,DEP-CSR-02,DEP-CSR-06,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-10a; release=family release point; split-from=N/A; exception=N/A`

---

### Task CSR-11: 修复 `-f` 删除路径逐组件符号链接不跟随与常规/索引回归（#712）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #712：`checkout -f <commit>`（`checkout.rs:1161` → `switch::checkout_to_commit`，`switch.rs:1412,1433`）与 `switch -f <branch>`（`switch.rs:1540,1607,1691`）都经 `restore_to_commit` → `restore::execute_safe` → `run_restore` → `restore_worktree_tracked`（`restore.rs:684`），在 `restore.rs:764` 调用 `remove_worktree_path_for_restore`（`:2045-2056`）删除目标树中不存在的已跟踪路径；该函数只对整串路径 `dir/f` 做 `symlink_metadata`，前导组件 `dir` 是符号链接时会穿过它删除外部的 `untracked/f`。本卡把删除改为对相对工作区根的**每一级路径组件**先做 `symlink_metadata` 检查（前导组件为符号链接时不穿过、不删除其目标内容），并让 `restore_worktree`（`:2164`，`:2206` 的同类 `remove_file`）复用同一检查（GC-02、GC-CSR-02）；同卡对两个入口验证结果状态与 `HEAD`。
**Out of scope:** 非 `--force` 下的前导目录符号链接写入（`CSR-14`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| 实际删除链：`restore_worktree_tracked`（`restore.rs:684`）在 `:764` 调用 `remove_worktree_path_for_restore`（`:2045-2056`），后者仅对整串路径做 `symlink_metadata`；`restore_worktree`（`:2164`）在 `:2206` 直接 `fs::remove_file` | `src/command/restore.rs:684,764,2045-2056,2164,2206` |
| 两个入口的调用链 | `src/command/checkout.rs:1161`、`src/command/switch.rs:1412,1433,1540,1607,1691` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout -f HEAD~1` 之后 `untracked/` 的内容与执行前相同。
- [ ] **AC2** `libra checkout -f HEAD~1` 之后结果状态（工作区与索引）等于 Git 期望（`a.txt` 为目标内容、无关未跟踪 `u.txt` 保留、索引等于目标树）。
- [ ] **AC3** `libra checkout -f HEAD~1` 之后 `HEAD` 分离于 C1。
- [ ] **AC4** `libra switch -f old` 之后 `untracked/` 的内容与执行前相同。
- [ ] **AC5** `libra switch -f old` 之后结果状态等于 Git 期望。
- [ ] **AC6** `libra switch -f old` 之后 `HEAD` 指向 `refs/heads/old`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F11（#712）：C1 含 `a.txt`=`A1\n`；C2（`main` 的 `HEAD`）把 `a.txt` 改为 `A2\n` 并新增已跟踪 `dir/f`=`F\n`；分支 `old` 指向 C1。随后在工作区执行 `mv dir untracked && ln -s untracked dir`（`dir` 成为指向仓库内未跟踪目录 `untracked/` 的符号链接，`untracked/f`=`F\n`），并新增无关未跟踪 `u.txt`=`U\n`；`S.ext` 取 `untracked/` 的清单（工作区清单 `S.wt` 按第 6 项不跟随符号链接，`untracked/` 同时出现在 `S.wt` 中）；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项；AC1–AC3 为同一次 `libra checkout -f HEAD~1`（`HEAD~1` 解析为提交而非分支，经 `checkout_detached` 进入 `switch::checkout_to_commit`，`src/command/checkout.rs:1154-1161`），AC4–AC6 为另一份全新夹具上的同一次 `libra switch -f old`（经 `move_to_resolved_branch`））
- AC1 → V1：exit 0；`S.ext`（`untracked/`）与运行前逐字相等。
- AC2 → V1：AC1 同一次调用后 `S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git checkout -f HEAD~1`；含 `a.txt`=`A1`、`u.txt`=`U`，`dir` 的去留以 Git 结果为准，#712 验收 2）。
- AC3 → V1：AC1 同一次调用后 `HEAD` 为分离状态且对象名等于 C1。
- AC4 → V2：exit 0；`S.ext` 与运行前逐字相等。
- AC5 → V2：AC4 同一次调用后 `S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git switch -f old`）。
- AC6 → V2：AC4 同一次调用后 `HEAD` 指向 `refs/heads/old`。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::force_checkout_does_not_follow_leading_symlink)'`（卡特有 `(new)`；覆盖 AC1–AC3；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::switch_test::force_switch_does_not_follow_leading_symlink)'`（卡特有 `(new)`；覆盖 AC4–AC6；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::switch_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::restore::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享工作区恢复模块 `src/command/restore.rs` 与 `src/command/switch.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-10R, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/checkout.md`、`docs/development/commands/switch.md`（`-f` 不经前导符号链接删除外部内容）、网站 `checkout.en.md` 与 `switch.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout`、`switch` 两行原地更新）。文档关键字（EN/zh 命令页）：`-f`（EN 另含 `symbolic link`，zh 另含 `符号链接`）。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=-f逐组件符号链接不跟随与常规/索引回归; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-10R,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-10R; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-12: 支持 `checkout -m`（合并切换）并创建冲突阶段（#713）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #713：在 `CheckoutArgs` 注册 `-m`/`--merge`，切换分支时对本地修改与目标分支做三方合并（复用 `merge` 内容合并实现，GC-02）：无冲突时切换并保留合并结果；有冲突时切换、写冲突标记并在索引记录 stage 1/2/3；随后 `checkout -f <branch>` 消解为 stage 0；`-f` 与 `-m` 互斥；合并写入失败时整体回滚。
**Out of scope:** `merge` 的 D/F `destdir~HEAD` 阶段（`CSR-21`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CheckoutArgs` 未注册 `-m`/`--merge` | `src/command/checkout.rs:53-120` |
| Git：`-f` 与 `-m` 互斥 | `git@6de20f6092:builtin/checkout.c:1706-1707` |
| 测试故障注入模式（`LIBRA_TEST=1` 下的环境变量注入点） | `src/command/tag.rs:889-900` |
**Acceptance criteria:**
- [ ] **AC1** 无冲突场景 `libra checkout -m other` 切换到 `other`。
- [ ] **AC2** 无冲突场景切换后的结果状态（工作区与索引）等于 Git 期望（`f.txt` 为合并结果）。
- [ ] **AC3** 冲突场景 `libra checkout -m other` 仍切换到 `other`。
- [ ] **AC4** 冲突场景工作区 `f.txt` 含冲突标记，字节等于 Git 期望。
- [ ] **AC5** 冲突场景索引中 `f.txt` 的 stage 1/2/3 条目等于 Git 期望。
- [ ] **AC6** 冲突后 `libra checkout -f other` 使 `f.txt` 只剩 stage 0 条目。
- [ ] **AC7** `libra checkout -f -m other` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC8** 合并写入索引后注入失败时以 `LBR-IO-002` 失败，且 `HEAD`、引用、索引与工作区（含本地修改）回到运行前。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F12：C0（`main`）含 `f.txt` = `1\n`…`10\n`；分支 `other`（C1）把第 2 行改为 `O2`；在 `main` 上的本地未提交修改：无冲突场景把第 9 行改为 `L9`，冲突场景把第 2 行改为 `L2`；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具（`git checkout -m other` 等），快照按第 6 项）
- AC1 → V1：exit 0；`S.ref` 中 `HEAD` 指向 `refs/heads/other`。
- AC2 → V1：AC1 同一次调用后 `S.wt` 与 `S.idx` 作为整体等于 Git 期望（`f.txt` 第 2 行 `O2`、第 9 行 `L9`）。
- AC3 → V1：`S.ref` 中 `HEAD` 指向 `refs/heads/other`（退出码按 Git 期望记录，不作判据）。
- AC4 → V1：`f.txt` 字节等于 Git 期望（含 `<<<<<<<`、`=======`、`>>>>>>>` 标记行）。
- AC5 → V1：`libra ls-files --stage -- f.txt` 的 stdout 等于 Git 期望（3 行，stage 1/2/3，blob 对象名同内容相同）。
- AC6 → V1：在 AC3 同一次 `checkout -m other` 之后（前置断言此时 `f.txt` 有 stage 1/2/3、`HEAD` 已指向 `other`）执行 `libra checkout -f other`；exit 0；`libra ls-files --stage -- f.txt` 恰 1 行且 stage 为 0，`f.txt` 字节等于 C1 内容。
- AC7 → V1：exit 非 0，stderr 含 `Error-Code: LBR-CLI-002`，`S` 不变。
- AC8 → V2：子进程环境 `LIBRA_TEST=1`、`LIBRA_TEST_CHECKOUT_MERGE_FAIL_AFTER_INDEX=1`（按「验证执行协议」第 8 项新增的注入点）；exit 非 0，stderr 含 `Error-Code: LBR-IO-002`；`S` 与运行前逐字相等。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::merge_checkout_creates_conflict_stages)'`（卡特有 `(new)`；覆盖 AC1–AC7；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::merge_checkout_failure_rolls_back)'`（卡特有 `(new)`；覆盖 AC8；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::checkout::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享切换/恢复模块 `src/command/switch.rs` 与 `src/command/restore.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-11, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`-m`/`--merge` 合并切换、冲突阶段与 `-f` 互斥）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--merge`。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=checkout-m合并切换与冲突恢复; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-11,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-11; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-13: 修复跟踪目录被文件替换后 `status` 报 `LBR-IO-001` 与普通 `checkout HEAD` 保留（#714 部分 1/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #714 中 `status` 与普通 `checkout HEAD` 轴：修复 `src/command/status.rs:745` 在已跟踪目录被普通文件替换后以 `LBR-IO-001`（`status output would be incomplete`）失败的问题，并保证普通 `libra checkout HEAD` 不破坏用户文件（保留文件 `dir`、`F.t` 编辑与当前分支）。
**Out of scope:** `checkout --force HEAD`/`switch -f` 强制恢复（`CSR-29`）；非空目录替换（`CSR-20`）；parallel-checkout worker 计数（`DEFER-CSR-02`）。G-09 拆分登记：本卡拆出 `CSR-29`。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `status` 在被替换的跟踪目录处报 `cannot inspect … status output would be incomplete` | `src/command/status.rs:745` |
**Acceptance criteria:**
- [ ] **AC1** `libra status` 退出 0。
- [ ] **AC2** `libra status --porcelain` 的 stdout 等于 Git 期望（含 ` D dir/file`）。
- [ ] **AC3** 普通 `libra checkout HEAD` 退出 0。
- [ ] **AC4** 普通 `libra checkout HEAD` 不改动工作区与索引（保留普通文件 `dir` 与 `F.t` 的本地编辑）。
- [ ] **AC5** 普通 `libra checkout HEAD` 后 `HEAD` 仍指向 `refs/heads/main`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F13（#714）：`main` 上 C1 含 `dir/file`=`D\n` 与 `F.t`=`T1\n`；工作区执行 `rm -r dir && printf 'X\n' > dir`（已跟踪目录被普通文件替换）并把 `F.t` 改为 `T2\n`；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 0；stderr 不含 `Error-Code: LBR-IO-001`。
- AC2 → V1：exit 0；stdout 逐字等于 Git 期望（`git status --porcelain`）。
- AC3 → V1：exit 0。
- AC4 → V1：AC3 同一次调用前后 `S.wt` 与 `S.idx` 逐字相等（含 `dir` 为普通文件 `X`、`F.t` 为 `T2`）。
- AC5 → V1：AC3 同一次调用后 `HEAD` 指向 `refs/heads/main`。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::tracked_dir_replaced_by_file_status_and_noop)'`（卡特有 `(new)`；覆盖 AC1–AC5；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::status_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::status::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享状态检查 `src/command/status.rs` 与 `src/command/restore.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-12, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/status.rs`, `src/command/checkout.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `docs/error-codes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（普通 `checkout HEAD` 不改动被文件替换的目录与本地编辑）、`docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`（该场景报告 `dir/file` 删除而非失败）、网站 `checkout.en.md` 与 `status.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout`、`status` 两行原地更新）；`docs/error-codes.md` 预期「无需修改」（本卡不新增稳定码，第 1 步记录理由与未变 SHA-256）。文档关键字（EN/zh 命令页）：`checkout HEAD`（checkout 页）、`dir/file`（status 页）。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=status宽容D/F与普通checkoutHEAD; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=3; prod-files=3; scope=M; deps=CSR-12,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-12; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-29: 支持 `checkout --force HEAD` / `switch -f` 恢复 D/F 与 `F.t`（#714 部分 2/2）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #714 中强制恢复轴（G-09：自 `CSR-13` 拆出）：`libra checkout --force HEAD` 与 `libra switch -f <branch>` 删除挡路的普通文件 `dir`、恢复 `dir/file`，并把 `F.t` 恢复为目标版本。修复点为按 ER-02 复现定位的实际失败入口（Issue #714 的 `local changes would be overwritten by checkout`）；不修改、也不放宽 `preflight_worktree_restore_to_commit` 对真实非空目录与子模块的保护（`restore.rs:1978-1996`），该保护由 `CSR-20` 的拒绝 AC 与共享回归组持续覆盖。
**Out of scope:** `status` 宽容与普通 `checkout HEAD`（`CSR-13`）；parallel-checkout worker 计数（`DEFER-CSR-02`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| Issue #714 复现中 `checkout --force HEAD` 以 `local changes would be overwritten by checkout`（`LBR-REPO-003`）失败，该文案来自 `DirtyUnstaged`/`DirtyUncommitted` 映射；`preflight_worktree_restore_to_commit` 的 `NonEmptyWorktreeDirectory` 只拦截真实的非空目录（非目录直接跳过），不是本场景根因。实际失败入口开工时按 ER-02 复现定位，修复不得放宽非空目录与子模块保护 | Issue #714、`src/command/checkout.rs:233-236`、`src/command/restore.rs:1933-1997`（`:1984-1996`）、`src/command/switch.rs:1433,1607` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout --force HEAD` 后结果状态（工作区与索引）等于 Git 期望（删除普通文件 `dir`、恢复 `dir/file`、`F.t` 回到 `HEAD` 版本）。
- [ ] **AC2** `libra checkout --force HEAD` 后 `HEAD` 仍指向 `refs/heads/main`。
- [ ] **AC3** `libra switch -f t` 后结果状态等于 Git 期望（删除普通文件 `dir`、恢复 `dir/file`、`F.t` 为 `t` 的版本、`z.txt` 不在工作区）。
- [ ] **AC4** `libra switch -f t` 后 `HEAD` 指向 `refs/heads/t`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F13（#714）：`main` 上 C1 含 `dir/file`=`D\n` 与 `F.t`=`T1\n`；工作区执行 `rm -r dir && printf 'X\n' > dir`（已跟踪目录被普通文件替换）并把 `F.t` 改为 `T2\n`；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项；`switch` 场景另建分支 `t` 指向 C1 并从 `main`（另一提交 C2，`dir/file`、`F.t` 与 C1 相同，另有 `z.txt`）执行；每组各用全新夹具）
- AC1 → V1：exit 0；`S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git checkout --force HEAD`）。
- AC2 → V1：AC1 同一次调用后 `HEAD` 指向 `refs/heads/main`。
- AC3 → V2：exit 0；`S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git switch -f t`）。
- AC4 → V2：AC3 同一次调用后 `HEAD` 指向 `refs/heads/t`。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::force_head_recovers_dir_and_ft)'`（卡特有 `(new)`；覆盖 AC1–AC2；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::switch_test::force_switch_recovers_dir_and_ft)'`（卡特有 `(new)`；覆盖 AC3–AC4；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::switch_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::restore::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享工作区恢复模块 `src/command/restore.rs` 与 `src/command/switch.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-13, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/checkout.md`、`docs/development/commands/switch.md`（`--force` 删除挡路文件并恢复跟踪内容）、网站 `checkout.en.md` 与 `switch.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout`、`switch` 两行原地更新）。文档关键字（EN/zh 命令页）：`--force`。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--force恢复D/F与F.t; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-13,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-13; release=independent; split-from=CSR-13; exception=N/A`

---

### Task CSR-14: 允许 `checkout --force` 替换前导目录符号链接为真实目录（#715）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #715：跟踪路径的前导目录在工作区是符号链接时，`checkout --force HEAD`（#715 验收场景）与 `switch -f <branch>` 只 unlink 该符号链接（不触碰其目标）并创建真实目录写入文件；不带 `--force` 时不得穿过符号链接写入外部目录（改为拒绝或不写）。写入点为 `restore_target_to_file_typed`（`restore.rs:1817`），其在 `:1839` 对父路径 `create_dir_all` 后直接写文件，父路径为符号链接时会写入外部目标（GC-CSR-02）。
**Out of scope:** parallel-checkout worker 计数（`DEFER-CSR-02`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| 普通切换的写入链 `restore_worktree_tracked`（`restore.rs:684`）→ `restore_target_to_file_typed`（`:1817`，`:1839` 对父路径 `create_dir_all` 后写入），不检查前导组件是否为符号链接；`validate_explicit_destination_path`（`:1235-1260`）只在显式 `destination_root` 时调用，普通 `run_restore` 传入 `None`（`:562`），不保护本场景 | `src/command/restore.rs:562,684,1235-1260,1817-1840`、`src/command/switch.rs:1433,1607` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout --force HEAD` 后结果状态（工作区与索引）等于 Git 期望（`dir` 为真实目录、`dir/file` 为 `F`）。
- [ ] **AC2** `libra checkout --force HEAD` 不改动外部目录 `<ext>`。
- [ ] **AC3** 不带 `--force` 的 `libra checkout t` 不改动外部目录 `<ext>`。
- [ ] **AC4** `libra switch -f t` 后结果状态（工作区与索引）等于 Git 期望（`dir` 为真实目录、`dir/file` 为 `F2`，索引记录 `F2`）。
- [ ] **AC5** `libra switch -f t` 不改动外部目录 `<ext>`。
- [ ] **AC6** `libra switch -f t` 后 `HEAD` 指向 `refs/heads/t`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F14：`main` 的 `HEAD` 含 `dir/file`=`F\n`；分支 `t` 自其分出并把 `dir/file` 改为 `F2\n`（目标树须写入 `dir/file`，用以区分是否穿过符号链接写入）；工作区执行 `rm -r dir`，在仓库外测试临时目录建 `<ext>/e`=`E\n`，再 `ln -s <ext> dir`；`S.ext` 取 `<ext>` 的清单；每个场景用全新夹具；Git 期望按「验证执行协议」第 4 项，快照按第 6 项）
- AC1 → V1：exit 0；`S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git checkout --force HEAD`；`dir` 的 `symlink_metadata` 类型为目录）。
- AC2 → V1：AC1 同一次调用前后 `S.ext` 逐字相等。
- AC3 → V1：无论退出码为何，调用前后 `S.ext` 逐字相等（`<ext>` 中不出现 `file`）。
- AC4 → V2：exit 0；`S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git switch -f t`）。
- AC5 → V2：AC4 同一次调用前后 `S.ext` 逐字相等。
- AC6 → V2：AC4 同一次调用后 `HEAD` 指向 `refs/heads/t`。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::force_replaces_leading_dir_symlink)'`（卡特有 `(new)`；覆盖 AC1–AC3；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::switch_test::force_replaces_leading_dir_symlink)'`（卡特有 `(new)`；覆盖 AC4–AC6；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::switch_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::restore::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享工作区恢复模块 `src/command/restore.rs` 与 `src/command/switch.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-29, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/checkout.md`、`docs/development/commands/switch.md`（`--force` 以真实目录替换前导符号链接，不触碰其目标）、网站 `checkout.en.md` 与 `switch.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout`、`switch` 两行原地更新）。文档关键字（EN/zh 命令页）：`--force`（EN 另含 `symbolic link`，zh 另含 `符号链接`）。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--force替换前导目录符号链接; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-29,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-29; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-15: 支持 `checkout --orphan <new-branch> [<start-point>]` 起点参数（#716）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #716（`REL-CSR-ORPHAN` 子卡 1/2，经 `CSR-15R` 联合发布）：移除 `run_checkout` 对 `checkout --orphan` 携带 `<start-point>` 的拒绝，把索引与工作区置为 `<start-point>` 的树，同时让 `HEAD` 指向 unborn `<new-branch>`；不传 `<start-point>` 时保持现有行为。
**Out of scope:** dirty 冲突保护与失败原子性（`CSR-15a`）；`switch --orphan`（保持既有契约）。G-09 拆分登记：本卡拆出 `CSR-15a`。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `run_checkout` 显式报错 `checkout --orphan does not accept a start-point`（`LBR-CLI-002`） | `src/command/checkout.rs:461-470` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout --orphan fresh C1` 后 `HEAD` 指向 unborn `refs/heads/fresh`。
- [ ] **AC2** 上述命令后索引等于 C1 的树。
- [ ] **AC3** 上述命令后工作区等于 C1 的树。
- [ ] **AC4** 随后首次 `libra commit -m root` 生成无父提交的 root commit。
- [ ] **AC5** `libra checkout --orphan bad nosuch` 以 `LBR-CLI-003` 拒绝。
- [ ] **AC6** `libra checkout --orphan fresh2`（无起点）保留索引与工作区。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-15R` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F15：`main` 上 C1（`a.txt`=`A1\n`、`n.txt` 不存在）→ C2=`HEAD`（`a.txt`=`A2\n`、新增 `b.txt`=`B\n`），另在 C1 上建分支 `s1`（再提交 C3 新增 `n.txt`=`N\n`）；工作区干净；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 0；`S.ref` 中 `HEAD` 符号指向 `refs/heads/fresh` 且 `reference` 表无 `refs/heads/fresh` 行。
- AC2 → V1：`S.idx` 等于 Git 期望（`git checkout --orphan fresh C1`）。
- AC3 → V1：`S.wt` 等于 Git 期望（`a.txt`=`A1`，无 `b.txt`）；对照：不带起点时 Git 结果保留 `b.txt`。
- AC4 → V1：在 AC1 同一次调用之后（前置断言 `HEAD` 指向 unborn `refs/heads/fresh`）执行 `libra commit -m root`；exit 0；`libra cat-file -p HEAD` 的输出不含以 `parent ` 开头的行。
- AC5 → V1：exit 非 0，stderr 含 `Error-Code: LBR-CLI-003`，`S` 不变。
- AC6 → V1：exit 0；`S.wt` 与 `S.idx` 与运行前逐字相等，`HEAD` 符号指向 `refs/heads/fresh2`。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::orphan_with_start_point)'`（卡特有 `(new)`；覆盖 AC1–AC6；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::checkout::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-15R` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-14, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`--orphan <new-branch> [<start-point>]`）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--orphan <new-branch> [<start-point>]`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-ORPHAN (1/2)
**C/D coverage from:** CSR-15R
**Granularity:** `type=implementation; axis=checkout--orphan接收start-point; recovery=revert本地提交; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CSR-14,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-14; release=family child; split-from=N/A; exception=N/A`

---

### Task CSR-15a: orphan 起点切换的 dirty 冲突保护与失败原子性

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** G-09：自 `CSR-15` 拆出的状态保护轴（`REL-CSR-ORPHAN` 子卡 2/2）：`<start-point>` 的树不得覆盖已暂存、未暂存或未跟踪内容；写入中途失败时完整回滚，不留半切换状态。
**Out of scope:** 起点填充与首次 root commit（`CSR-15`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CSR-15` 引入起点后需与普通切换相同的覆盖检查与原子性 | `src/command/checkout.rs:461-470`、`src/command/tag.rs:889-900`（注入点模式） |
**Acceptance criteria:**
- [ ] **AC1** 已暂存 `a.txt` 修改会被起点树覆盖时，`libra checkout --orphan o1 C1` 以 `LBR-CONFLICT-002` 拒绝。
- [ ] **AC2** 未暂存 `a.txt` 修改会被覆盖时以 `LBR-CONFLICT-002` 拒绝。
- [ ] **AC3** 未跟踪 `n.txt` 会被 `s1` 的起点树覆盖时，`libra checkout --orphan o3 s1` 以 `LBR-CONFLICT-002` 拒绝。
- [ ] **AC4** AC1 场景的 stderr 列出冲突路径 `a.txt`。
- [ ] **AC5** 写入索引后注入失败时以 `LBR-IO-002` 失败，且 `HEAD`、引用、索引与工作区回到运行前。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–4 步与第 8 步（家族子卡口径）逐文件交付并验收；网站第 5–7 步由唯一发布点 `CSR-15R` 执行（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F15：`main` 上 C1（`a.txt`=`A1\n`、`n.txt` 不存在）→ C2=`HEAD`（`a.txt`=`A2\n`、新增 `b.txt`=`B\n`），另在 C1 上建分支 `s1`（再提交 C3 新增 `n.txt`=`N\n`）；工作区干净；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项；AC1 把 `a.txt` 改为 `S\n` 并 `libra add`，AC2 把 `a.txt` 改为 `W\n`（不暂存），AC3 新建未跟踪 `n.txt`=`U\n`，AC5 在干净工作区以子进程环境 `LIBRA_TEST=1`、`LIBRA_TEST_CHECKOUT_ORPHAN_FAIL_AFTER_INDEX=1` 运行 `libra checkout --orphan o5 C1`）
- AC1 → V1：exit 非 0，stderr 含 `Error-Code: LBR-CONFLICT-002`，`S` 不变。
- AC2 → V1：判定同 AC1。
- AC3 → V1：判定同 AC1（`n.txt` 仍为 `U\n`）。
- AC4 → V1：AC1 那次调用的 stderr 含 `a.txt`。
- AC5 → V1：exit 非 0，stderr 含 `Error-Code: LBR-IO-002`；`S` 与运行前逐字相等（`HEAD` 仍指向 `refs/heads/main`）。
- 文档 AC → 「文档与网站验收命令」第 1–4 步与第 8 步家族子卡口径（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::orphan_start_point_dirty_conflict_and_rollback)'`（卡特有 `(new)`；覆盖 AC1–AC5；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** N/A（family child，由 `CSR-15R` 统一执行 `T-2`；本卡自跑 `fmt`、`clippy` 与本卡 A 组 focused 门）
**Dependencies:** CSR-15, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** N/A
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`--orphan` 带起点时的覆盖保护）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`--orphan`。
**Rollback mode:** revert（家族窗口内对本卡本地签名提交执行 `libra revert`；家族发布后由发布点按 `immutable-release` 前滚）
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** N/A
**Release boundary:** family child of REL-CSR-ORPHAN (2/2)
**C/D coverage from:** CSR-15R
**Granularity:** `type=implementation; axis=orphan起点dirty保护与失败原子性; recovery=revert本地提交; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CSR-15,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-15; release=family child; split-from=CSR-15; exception=N/A`

---

### Task CSR-15R: `REL-CSR-ORPHAN` 起点与 dirty 保护联合发布点

**Task type:** release
**Lifecycle / Acceptance:** pending / 空
**Description:** 聚合 `CSR-15`, `CSR-15a`（`REL-CSR-ORPHAN`），在全部子卡本地签名提交并通过本地 A/B、ER-05 与 fmt/clippy 后，统一执行 C 组（`patch + 1`、版本面三处同步、工具链刷新 `Cargo.lock`、T-2 全量、签名提交、PR 与 squash merge、`gh release create`）、D 组与家族网站第 5–7 步，作为该家族唯一发布点（G-08）。
**Out of scope:** 不引入新产品代码或文档内容改动。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CSR-15`, `CSR-15a` 同属一个公开行为面，须同版发布 | 「发布分组与并发窗口」`REL-CSR-ORPHAN` |
**Acceptance criteria:**
- [ ] **AC1** `CSR-15` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC2** `CSR-15a` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC3** 聚合守卫：家族全部卡特有用例（下方「聚合守卫」所列）在已 bump 的发布树上逐条按「验证执行协议」第 1–3 项通过。
- [ ] **AC4** `gh release view v<version>` 的 release note 正文逐条列出 `CSR-15` 与 `CSR-15a` 的用户可见变化。
- [ ] **AC5** 每张子卡在「文档与网站验收命令」第 1 步登记的每个仓库文档改动后 SHA-256，等于该子卡本地签名提交中同一文件的 SHA-256。
- [ ] **AC6** 每个被家族子卡修改的仓库文档在发布树中的 SHA-256，等于家族顺序中最后修改它的子卡的登记值。
- [ ] **AC7** 家族网站页 `checkout.en.md` 按「文档与网站验收命令」第 2–7 步完成（第 3 步以本卡合入 `main` 后的家族最终 EN 文档同步，第 8 项家族口径）。
（ER-04 的 fmt / clippy / T-2 全量与 ER-08 版本面门按 G-03 不计入本卡特有 AC/VER 分子。）
**AC → 断言映射:**（非测试型判据按「验证执行协议」第 9 项；`<REL>` 为本卡 C 组第⑦步签名提交 SHA，`<子卡 提交>` 为各子卡本地签名提交 SHA（登记于子卡记录））
- AC1 → V1：`libra merge-base --is-ancestor <CSR-15 提交> <REL>` exit 0。
- AC2 → V2：`libra merge-base --is-ancestor <CSR-15a 提交> <REL>` exit 0。
- AC3 → 聚合守卫：所列每条命令按协议第 1–3 项判定全部通过（ER-04 B 组 `release` 类型门），逐条记录 exit code 与计数。
- AC4 → V3：`gh release view v<version> --json body --jq .body` exit 0，正文对 `CSR-15` 与 `CSR-15a` 各有一条变化说明（逐条以卡 ID 标注）。
- AC5 → V4：对每张子卡登记的每个文档执行 `libra show <该子卡提交>:<路径> | shasum -a 256`，逐一等于其登记值；任一不等即失败。
- AC6 → V5：对每个文档取最后修改它的子卡登记值，与 `libra show <REL>:<路径> | shasum -a 256` 比较相等；任一不等即失败。
- AC7 → V6：第 2–7 步逐步记录命令、退出码与 SHA-256；任一步 STOP 即不成立。
**Verification:**
- [ ] V1 `libra merge-base --is-ancestor <CSR-15 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V2 `libra merge-base --is-ancestor <CSR-15a 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V3 `gh release view v<version> --json body --jq .body`（非测试型，按协议第 9 项；exit 0 且逐卡说明齐全为通过）
- [ ] V4 `libra show <子卡提交>:<路径> | shasum -a 256`（逐子卡逐文件；非测试型，按协议第 9 项）
- [ ] V5 `libra show <REL>:<路径> | shasum -a 256`（逐文件；非测试型，按协议第 9 项）
- [ ] V6 「文档与网站验收命令」第 2–7 步（非测试型，按协议第 9 项；各步判据见该节）
**聚合守卫（ER-04 B 组 `release` 类型门，不计入 VER）：** `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::orphan_with_start_point)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::orphan_start_point_dirty_conflict_and_rollback)'`
**Full-suite trigger:** T-2：`family release point`（C 组第④步在 bump 后的精确提交树上跑 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 与 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，记录提交 SHA 与 `HEAD^{tree}`；合入与发布前核对发布树与已测树一致，变化即重跑）
**Dependencies:** CSR-15a, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `docs/development/plan/plan-20261117-checkout-switch-restore.md`（发布证据登记）, `tests/INDEX.md`（仅需同步时）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`（第 6 步提交）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 版本面三处（`Cargo.toml`、`install.sh`、`install.ps1`）与工具链刷新的 `Cargo.lock`（属 `Release write set`）
**Docs and compatibility impact:** 不新增文档内容：汇总子卡文档交付证据（AC5、AC6），以家族最终 EN 文档完成网站 `checkout.en.md` 第 2–7 步（AC7）。
**Rollback mode:** immutable-release
**Estimated scope:** S（0 个生产文件，仅家族发布聚合）
**Version increment:** patch
**Release boundary:** family release point of REL-CSR-ORPHAN
**C/D coverage from:** self
**Granularity:** `type=release; axis=REL-CSR-ORPHAN家族发布点; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/12; VER=6/12; landing=0; prod-files=0; scope=S; deps=CSR-15a,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-15a; release=family release point; split-from=N/A; exception=N/A`

---

### Task CSR-16: 修复 unborn orphan 分支上 `checkout -b <new>` 成功切换（#718）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #718：修复 `resolve_checkout_create_startpoint`（`src/command/checkout.rs:960-969`）在当前处于 unborn orphan 分支且未给 `<start-point>` 时执行 `libra checkout -b <new>` 报错的问题，使 `HEAD` 改指向新的 unborn 分支 `<new>`，原 unborn 分支不留下引用；有提交时行为不变。
**Out of scope:** `checkout --orphan <start-point>`（`CSR-15`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `resolve_checkout_create_startpoint` 在 `Head::current_commit_result()` 为 `None` 时返回 `InvalidObjectName` | `src/command/checkout.rs:960-969` |
**Acceptance criteria:**
- [ ] **AC1** 在 unborn orphan 分支 `o` 上执行 `libra checkout -b another` 退出 0。
- [ ] **AC2** 上述命令后 `HEAD` 指向 unborn `refs/heads/another`。
- [ ] **AC3** 上述命令后不存在 `refs/heads/o` 引用。
- [ ] **AC4** 在有提交的 `main` 上 `libra checkout -b n2` 后 `refs/heads/n2` 指向当前提交 C1。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F16：`main` 上提交 C1（`a.txt`=`A\n`）；随后 `libra checkout --orphan o`（无起点，索引与工作区保留 C1 内容）；AC4 在另一份只含 `main`/C1 的全新夹具上执行；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 0（Git 期望同为 0）。
- AC2 → V1：`S.ref` 中 `HEAD` 符号指向 `refs/heads/another`，`reference` 表无 `refs/heads/another` 行；`S.wt` 与 `S.idx` 不变。
- AC3 → V1：`reference` 表无 `refs/heads/o` 行。
- AC4 → V1：exit 0；`libra rev-parse refs/heads/n2` 等于 C1。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_b_from_unborn_orphan)'`（卡特有 `(new)`；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::checkout::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`）；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-15R, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（unborn 分支上 `-b` 只改 `HEAD` 指向）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`unborn`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=unborn上checkout-b成功; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CSR-15R,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-15R; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-17: 修复多远端同名分支时 DWIM 歧义不再回退为 pathspec（#721）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #721：裸名称 `<name>` 在多个远端均有远程追踪分支时，`checkout <name>` 与 `switch <name>` 报告 `'<name>' matched multiple (<N>) remote tracking branches`（沿用 `switch.rs:264-268` 的 `LBR-CONFLICT-002` 映射，错误文案补计数），`checkout` 不再回退为 pathspec 改写同名文件。
**Out of scope:** 唯一远端匹配的 DWIM（`CSR-19`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `disambiguate_path_argument` 多远端匹配失败后回落为路径；`switch.rs:179` 文案无计数 | `src/command/checkout.rs:664-694`、`src/command/switch.rs:179,264-268,655` |
| Git：多于一个远端匹配时 die `matched multiple (%d)` | `git@6de20f6092:builtin/checkout.c:1401-1406` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout topic`（`origin/topic` 与 `upstream/topic` 均存在、工作区有已修改的已跟踪文件 `topic`）以 `LBR-CONFLICT-002` 拒绝（不回退为 pathspec，文件 `topic` 不被改写）。
- [ ] **AC2** 上述拒绝的 stderr 含 `'topic' matched multiple (2) remote tracking branches`。
- [ ] **AC3** 显式 `libra checkout -- topic` 仍按 pathspec 恢复文件 `topic`。
- [ ] **AC4** `libra switch topic` 以 `LBR-CONFLICT-002` 拒绝。
- [ ] **AC5** `libra switch topic` 的 stderr 含 `'topic' matched multiple (2) remote tracking branches`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F17：两个作为远端的 Libra 仓库 O1、O2 各有分支 `topic`（O1 的提交树为 `o1.txt`=`1\n`，O2 的为 `o2.txt`=`2\n`）；R 先 `libra init` 并提交 Cr（树：已跟踪文件 `topic`=`T\n`）作为 `main`，再 `libra remote add origin <O1>`、`libra remote add upstream <O2>` 并分别 `libra fetch origin`、`libra fetch upstream`；R 无本地 `topic` 分支，工作区把文件 `topic` 改为 `M\n`；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 非 0，stderr 含 `Error-Code: LBR-CONFLICT-002`，`S` 不变（工作区 `topic` 仍为 `M\n`）。
- AC2 → V1：AC1 同一次调用的 stderr 含该文案。
- AC3 → V1：exit 0；工作区 `topic` 字节为 `T\n`。
- AC4 → V2：exit 非 0，stderr 含 `Error-Code: LBR-CONFLICT-002`，`S` 不变。
- AC5 → V2：AC4 同一次调用的 stderr 含该文案。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::dwim_ambiguous_remote_does_not_fallback_to_pathspec)'`（卡特有 `(new)`；覆盖 AC1–AC3；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::switch_test::dwim_ambiguous_remote)'`（卡特有 `(new)`；覆盖 AC4–AC5；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::switch_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::switch::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享 DWIM 解析 `src/command/switch.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-16, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/checkout.md`、`docs/development/commands/switch.md`（多远端歧义报错带计数，不回退为 pathspec）、网站 `checkout.en.md` 与 `switch.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout`、`switch` 两行原地更新）。文档关键字（EN/zh 命令页）：`matched multiple`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=多远端DWIM歧义不回退pathspec; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CSR-16,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-16; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-18: 允许 `checkout -B` 在保留已暂存可合并改动下重置分支（#723）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #723：调整 `switch::ensure_switch_clean_or_force`（`src/command/switch.rs:852-867`）与 `checkout.rs:977-1000` 的 `-B` 路径，允许 `libra checkout -B <branch> <start-point>` 在已暂存改动可与目标提交安全合并时重置分支并保留该暂存改动；冲突时仍拒绝，`checkout -b`/`switch -C` 的共享预检保持拒绝冲突。
**Out of scope:** 带 `-f`/`-m` 的强制或合并切换（`CSR-12` 等）；干净工作区下 `-B` 的既有行为由共享回归组 `command::checkout_test` 覆盖，不另设卡特有 AC。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `ensure_switch_clean_or_force` 拒绝任何脏状态，连带阻塞 `-B` 保留可合并暂存 | `src/command/switch.rs:852-867`、`src/command/checkout.rs:977-1000` |
**Acceptance criteria:**
- [ ] **AC1** 可合并场景 `libra checkout -B topic C0` 把 `refs/heads/topic` 重置到 C0。
- [ ] **AC2** 可合并场景该命令后 `HEAD` 仍指向 `refs/heads/topic`（不分离）。
- [ ] **AC3** 可合并场景该命令后的结果状态（索引与工作区）等于 Git 期望（保留已暂存的 `a.txt`，`b.txt` 为 C0 内容）。
- [ ] **AC4** 冲突场景 `libra checkout -B topic C0` 以 `LBR-CONFLICT-002` 拒绝。
- [ ] **AC5** 冲突场景 `libra checkout -b nb C0` 以 `LBR-CONFLICT-002` 拒绝。
- [ ] **AC6** 冲突场景 `libra switch -C nb2 C0` 以 `LBR-CONFLICT-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F18（#723：重置当前分支）：C0 含 `a.txt`=`A\n`、`b.txt`=`B0\n`；C1 把 `b.txt` 改为 `B1\n`；当前分支为 `topic`（`HEAD` 指向 `refs/heads/topic`，`topic` 指向 C1）；可合并场景把 `a.txt` 改为 `AS\n` 并 `libra add`；冲突场景把 `b.txt` 改为 `BS\n` 并 `libra add`；每个 AC 用全新夹具；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 0；`libra rev-parse refs/heads/topic` 等于 C0。
- AC2 → V1：AC1 同一次调用后 `HEAD` 指向 `refs/heads/topic`（`kind = 'Head'` 行 `name = 'topic'`、`commit IS NULL`）。
- AC3 → V1：AC1 同一次调用后 `S.idx` 与 `S.wt` 作为整体等于 Git 期望（`git checkout -B topic C0`；`a.txt` 为 `AS\n` 且已暂存，`b.txt` 为 `B0\n`）。
- AC4 → V1：exit 非 0，stderr 含 `Error-Code: LBR-CONFLICT-002`，`S` 不变。
- AC5 → V2：exit 非 0，stderr 含 `Error-Code: LBR-CONFLICT-002`，`S` 不变（无 `refs/heads/nb`）。
- AC6 → V2：判定同 AC5（无 `refs/heads/nb2`）。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::force_branch_b_preserves_staged_mergeable_changes)'`（卡特有 `(new)`；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::switch_test::checkout_b_and_switch_c_conflict_preserves_state)'`（卡特有 `(new)`；覆盖 AC5–AC6；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::switch_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::switch::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享切换预检 `src/command/switch.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-17, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`-B` 保留可合并的已暂存改动）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`-B`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=checkout-B保留可合并暂存与共享预检回归; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CSR-17,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-17; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-19: 支持 `checkout <branch>` 从唯一远程追踪引用 DWIM 解析（#724）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #724：移除 `checkout.rs:1002-1022` 中 `get_remote` 硬编码 `origin/<branch>` 并强制 `pull` 的逻辑，改为复用 `switch.rs:655` 的全远端 DWIM（含 `checkout.defaultRemote`，GC-02），以本地已有远程追踪引用为准创建并追踪本地分支（与 Git 一致），不再在 checkout 中执行 `pull`（`--json` 的 `pulled` 恒为 `false`）。
**Out of scope:** 多远端歧义（`CSR-17`）；不新增 CLI 标志。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `get_remote` 硬编码 `origin/{branch}` 并执行 `pull` | `src/command/checkout.rs:1002-1022` |
| `switch` 已有全远端唯一匹配与 `checkout.defaultRemote` | `src/command/switch.rs:655-710` |
| `--json` 输出含 `pulled` 与 `tracking.remote` | `src/command/checkout.rs:122-143`、`tests/command/checkout_test.rs:787` |
**Acceptance criteria:**
- [ ] **AC1** 仅 `upstream/feat` 存在时 `libra checkout feat` 创建指向 `upstream/feat` 提交的本地 `feat`。
- [ ] **AC2** 上述命令后 `HEAD` 指向 `refs/heads/feat`。
- [ ] **AC3** 上述新建的 `feat` 的上游为 `upstream/feat`。
- [ ] **AC4** `origin/feat` 与 `upstream/feat` 都存在且 `checkout.defaultRemote=upstream` 时，`libra checkout feat` 以 `upstream/feat` 创建 `feat`。
- [ ] **AC5** `libra checkout feat` 以本地已有远程追踪引用为准：U 在 R fetch 之后前进到 U2 时，新建的 `feat` 与 `refs/remotes/upstream/feat` 都仍指向 U1。
- [ ] **AC6** `libra --json checkout feat` 的 `data.pulled` 为 `false`。
- [ ] **AC7** `libra --json checkout feat` 的 `data.tracking.remote` 为 `upstream`。
- [ ] **AC8** AC1 场景切换后的结果状态（工作区与索引）等于 Git 期望（`upstream/feat` 的树）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F19：作为远端的 Libra 仓库 U（分支 `feat` 指向 U1，U1 的树只含 `f.txt`=`F1\n`）与 O（分支 `feat` 指向不同提交 O1，树只含 `f.txt`=`O1\n`）；R 当前分支 `main` 的树只含 `r.txt`=`R\n`（与 U1 的树不同，切换必须删除 `r.txt` 并写入 `f.txt`）；R 中 `libra remote add upstream <U>` 并 `libra fetch upstream`；AC4 另 `libra remote add origin <O>` 并 `libra fetch origin`、`libra config set checkout.defaultRemote upstream`；R 无本地 `feat`；AC5 在 R fetch 之后于 U 的 `feat` 上再提交 U2；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 0；`libra rev-parse refs/heads/feat` 等于 `libra rev-parse refs/remotes/upstream/feat`（U1）。
- AC2 → V1：AC1 同一次调用后 `HEAD` 符号指向 `refs/heads/feat`。
- AC3 → V1：AC1 同一次调用后 `S.cfg` 中 `branch.feat.remote`=`upstream`、`branch.feat.merge`=`refs/heads/feat`（等于 Git 期望）。
- AC4 → V1：exit 0；`refs/heads/feat` 等于 U1（不等于 O1）。
- AC5 → V1：exit 0；`refs/heads/feat` 与 `refs/remotes/upstream/feat` 均等于 U1（等于 Git 期望；若实现在 checkout 中拉取则会成为 U2）。
- AC6 → V1：`libra --json checkout feat` exit 0；stdout 解析为 JSON 后 `data.pulled == false`。
- AC7 → V1：AC6 同一次调用的 `data.tracking.remote == "upstream"`。
- AC8 → V1：AC1 同一次调用后 `S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git checkout feat`）。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::dwim_single_non_origin_remote)'`（卡特有 `(new)`；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::switch::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享 DWIM 解析 `src/command/switch.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-18, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（任意唯一远端 DWIM、`checkout.defaultRemote`、不再自动 `pull`，`--json` 的 `pulled`/`tracking`）、网站 `checkout.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout` 行原地更新）。文档关键字（EN/zh 命令页）：`checkout.defaultRemote`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=单一远程引用DWIM解析; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=CSR-18,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-18; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-20: 允许普通 `checkout`/`switch` 用跟踪文件替换非空工作区目录（#1008 部分 1/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #1008 (a)：目标提交把目录 `destdir` 变为普通跟踪文件时，普通 `checkout`/`switch` 先删除将被移除的已跟踪文件与空目录、再写入新文件（与 `reset --hard` 一致）；目录内有未跟踪文件时 fail-closed 拒绝并保留内容。
**Out of scope:** `--force` 替换（`CSR-30`）；`merge` 的 `destdir~HEAD` 阶段（`CSR-21`）。G-09 拆分登记：本卡拆出 `CSR-30`。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| 写回文件遇到非空目录直接拒绝（`refusing to replace non-empty worktree directory`，`LBR-CONFLICT-002`） | `src/command/restore.rs:1601-1615,1933-1997`、`src/command/switch.rs:1433,1607` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout -q rd^0` 后结果状态（工作区与索引）等于 Git 期望（非空目录 `destdir/` 被替换为文件 `destdir`，`keep.txt` 的本地修改与无关未跟踪 `n.txt` 保留）。
- [ ] **AC2** `libra switch rdb` 后结果状态等于 Git 期望。
- [ ] **AC3** `destdir/` 含未跟踪文件时 `libra checkout -q rd^0` 以 `LBR-CONFLICT-002` 拒绝。
- [ ] **AC4** `destdir/` 含未跟踪文件时 `libra switch rdb` 以 `LBR-CONFLICT-002` 拒绝。
- [ ] **AC5** AC1 场景 `HEAD` 分离于 `rd`。
- [ ] **AC6** AC2 场景 `HEAD` 指向 `refs/heads/rdb`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F20（#1008，`t6402-merge-rename.sh#21` 的 setup）：`base`=`base\n` 提交后 `libra checkout --orphan rd && libra rm -rf .`；`one/file`=`stuff\n` 与 `keep.txt`=`K\n` 提交为 Common；`libra mv one/file destdir` 提交为 `rd`（`destdir` 为文件）；`libra checkout -b source-conflict HEAD~1 && libra rm -rf one`，再建 `one`（空文件）与 `destdir/foo`（空文件）并 `libra add -A` 提交为 `source-conflict`；另以 `libra branch rdb rd` 建分支 `rdb`；当前位于 `source-conflict`，工作区把 `keep.txt` 改为 `KL\n` 并新建无关未跟踪 `n.txt`=`N\n`；未跟踪场景另建 `destdir/u.txt`=`U\n`；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 0；`S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git checkout -q rd^0`；`destdir` 为文件 `stuff`、`keep.txt` 为 `KL`、`n.txt` 为 `N`）。
- AC2 → V2：exit 0；`S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git switch rdb`）。
- AC3 → V1：exit 非 0，stderr 含 `Error-Code: LBR-CONFLICT-002`，`S` 不变（`destdir/u.txt` 仍为 `U\n`）。
- AC4 → V2：判定同 AC3。
- AC5 → V1：AC1 同一次调用后 `HEAD` 为分离状态且对象名等于 `libra rev-parse rd`。
- AC6 → V2：AC2 同一次调用后 `HEAD` 指向 `refs/heads/rdb`。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::replace_non_empty_tracked_dir_with_file)'`（卡特有 `(new)`；覆盖 AC1、AC3、AC5；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::switch_test::replace_non_empty_tracked_dir_with_file)'`（卡特有 `(new)`；覆盖 AC2、AC4、AC6；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::switch_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::restore::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享工作区恢复模块 `src/command/restore.rs` 与 `src/command/switch.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-19, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/checkout.md`、`docs/development/commands/switch.md`（目录被跟踪文件替换、未跟踪内容时拒绝）、网站 `checkout.en.md` 与 `switch.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout`、`switch` 两行原地更新）。文档关键字（EN/zh 命令页）：`destdir`。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=普通模式非空目录替换为跟踪文件; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-19,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-19; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-30: 支持 `--force` 替换非空工作区目录与清理空目录（#1008 部分 2/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #1008 中 `--force` 轴（G-09：自 `CSR-20` 拆出）：`destdir/` 含阻塞普通切换的条目（含未跟踪文件）时，`checkout --force` 与 `switch -f` 按 Git 删除该目录并写入文件 `destdir`，不留残余空目录。
**Out of scope:** 普通模式（`CSR-20`）；`merge` 的 `destdir~HEAD`（`CSR-21`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `preflight_worktree_restore_to_commit` 忽略 `--force`，`restore.rs:1601-1615` 未在 `--force` 下先清理目录 | `src/command/restore.rs:1601-1615,1933-1997`、`src/command/switch.rs:1433,1607` |
**Acceptance criteria:**
- [ ] **AC1** `libra checkout --force rd^0` 把含未跟踪文件的 `destdir/` 替换为文件 `destdir`。
- [ ] **AC2** 上述运行后结果状态（工作区与索引）等于 Git 期望（不留 `destdir/` 下的残余条目）。
- [ ] **AC3** `libra switch -f rdb` 把含未跟踪文件的 `destdir/` 替换为文件 `destdir`。
- [ ] **AC4** 上述运行后结果状态（工作区与索引）等于 Git 期望。
- [ ] **AC5** `libra checkout --force rd^0` 后 `HEAD` 分离于 `rd`。
- [ ] **AC6** `libra switch -f rdb` 后 `HEAD` 指向 `refs/heads/rdb`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F20（#1008，`t6402-merge-rename.sh#21` 的 setup）：`base`=`base\n` 提交后 `libra checkout --orphan rd && libra rm -rf .`；`one/file`=`stuff\n` 与 `keep.txt`=`K\n` 提交为 Common；`libra mv one/file destdir` 提交为 `rd`（`destdir` 为文件）；`libra checkout -b source-conflict HEAD~1 && libra rm -rf one`，再建 `one`（空文件）与 `destdir/foo`（空文件）并 `libra add -A` 提交为 `source-conflict`；另以 `libra branch rdb rd` 建分支 `rdb`；当前位于 `source-conflict`，工作区把 `keep.txt` 改为 `KL\n` 并新建无关未跟踪 `n.txt`=`N\n`；未跟踪场景另建 `destdir/u.txt`=`U\n`；本卡各场景均含未跟踪 `destdir/u.txt`，每个命令用全新夹具；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 0；`destdir` 为普通文件且字节为 `stuff\n`。
- AC2 → V1：AC1 同一次调用后 `S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git checkout --force rd^0`）。
- AC3 → V2：exit 0；`destdir` 为普通文件且字节为 `stuff\n`。
- AC4 → V2：AC3 同一次调用后 `S.wt` 与 `S.idx` 作为整体等于 Git 期望（`git switch -f rdb`）。
- AC5 → V1：AC1 同一次调用后 `HEAD` 为分离状态且对象名等于 `libra rev-parse rd`。
- AC6 → V2：AC3 同一次调用后 `HEAD` 指向 `refs/heads/rdb`。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::force_replace_non_empty_dir_with_file)'`（卡特有 `(new)`；覆盖 AC1–AC2、AC5；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::switch_test::force_replace_non_empty_dir_with_file)'`（卡特有 `(new)`；覆盖 AC3–AC4、AC6；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::switch_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::restore::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享工作区恢复模块 `src/command/restore.rs` 与 `src/command/switch.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-20, DEP-CSR-02, DEP-CSR-05, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/checkout.rs`, `src/command/switch.rs`, `src/command/restore.rs`, `tests/command/checkout_test.rs`, `tests/command/switch_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/checkout.md`、`docs/development/commands/switch.md`（`--force` 替换非空目录并清理）、网站 `checkout.en.md` 与 `switch.en.md`、`COMPATIBILITY.md`（W-1 定位 `checkout`、`switch` 两行原地更新）。文档关键字（EN/zh 命令页）：`--force`。
**Rollback mode:** immutable-release
**Estimated scope:** M（3 个落点，3 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=--force替换非空目录与清理; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CSR-20,DEP-CSR-02,DEP-CSR-05,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-20; release=independent; split-from=CSR-20; exception=N/A`

---

### Task CSR-21: 修复 `merge` D/F 冲突时 `destdir~HEAD` 记录 stage 1 与 stage 2（#1008 部分 3/3）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #1008 (b)：`libra merge --strategy=recursive` 在 rename/delete 与 `destdir~HEAD` D/F 冲突同时发生时，把 `destdir~HEAD` 记为 stage 1 与 stage 2（对齐 Git），并不再额外输出 `CONFLICT (modify/delete): destdir~HEAD …`；常规 modify/delete 冲突输出不变。
**Out of scope:** 非空目录替换（`CSR-20`）；`merge-pull` 计划的其它合并语义（`DEP-CSR-04`）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| D/F 移位路径只写 stage 2 并追加 `modify/delete` 提示 | `src/command/merge.rs:4031`、`src/command/merge/rename_merge.rs` |
| Git 在同一场景 `ls-files -u` 为 2 行（stage 1、2），上游断言 `test_stdout_line_count = 2 git ls-files -u` | Issue #1008、`git@6de20f6092:t/t6402-merge-rename.sh`（#21） |
**Acceptance criteria:**
- [ ] **AC1** #1008 场景 `libra merge --strategy=recursive source-conflict` 以 `LBR-CONFLICT-002` 冲突结束（合并冲突现有映射 `src/command/merge.rs:1021-1026`）。
- [ ] **AC2** #1008 场景合并后 `libra ls-files -u` 的 stdout 等于 Git 期望（`destdir~HEAD` 的 stage 1 与 stage 2 两行）。
- [ ] **AC3** #1008 场景 `libra merge` 输出的 `CONFLICT (` 行集合等于 Git 期望（不含 `modify/delete` 的 `destdir~HEAD` 行）。
- [ ] **AC4** 常规 modify/delete 冲突仍输出与 Git 一致的 `CONFLICT (modify/delete)` 行。
- [ ] **AC5** #1008 场景合并后工作区清单等于 Git 期望。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F20（#1008，`t6402-merge-rename.sh#21` 的 setup）：`base`=`base\n` 提交后 `libra checkout --orphan rd && libra rm -rf .`；`one/file`=`stuff\n` 与 `keep.txt`=`K\n` 提交为 Common；`libra mv one/file destdir` 提交为 `rd`（`destdir` 为文件）；`libra checkout -b source-conflict HEAD~1 && libra rm -rf one`，再建 `one`（空文件）与 `destdir/foo`（空文件）并 `libra add -A` 提交为 `source-conflict`；另以 `libra branch rdb rd` 建分支 `rdb`；按 Issue 复现执行 `libra checkout --detach source-conflict && libra reset --hard rd^0 && libra merge --strategy=recursive source-conflict`；常规场景另建：基提交 `f.txt`=`f\n`，`ours` 改为 `f2\n`，`theirs` 删除 `f.txt`，在 `ours` 上 `libra merge theirs`；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 非 0，stderr 含 `Error-Code: LBR-CONFLICT-002`；这是「冲突结束」，按「验证执行协议」第 4(c) 项不要求 `S` 不变，状态改由 AC2、AC5 断言（上游同场景为 `test_must_fail git merge`）。
- AC2 → V1：AC1 同一次调用后 `libra ls-files -u` 的 stdout 逐字等于 Git 期望（`git ls-files -u`）。
- AC3 → V1：AC1 同一次调用 stdout 中以 `CONFLICT (` 开头的行组成的集合等于 Git 期望的同类集合。
- AC4 → V2：常规场景合并（冲突结束）stdout 中以 `CONFLICT (modify/delete)` 开头的行逐字等于 Git 期望。
- AC5 → V1：AC1 同一次调用后 `S.wt` 等于 Git 期望（`destdir/foo`、`one` 与 `destdir~HEAD` 内容同 Git）。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::df_merge_records_stage_1_and_2)'`（卡特有 `(new)`；覆盖 AC1–AC3、AC5；按「验证执行协议」第 1–4 项）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::modify_delete_conflict_still_reported)'`（卡特有 `(new)`；覆盖 AC4；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::merge::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享合并引擎 `src/command/merge.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-30, DEP-CSR-02, DEP-CSR-04, DEP-CSR-07, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/merge.rs`, `src/command/merge/rename_merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`（D/F 移位路径 `<path>~HEAD` 记录 stage 1/2）、网站 `merge.en.md`、`COMPATIBILITY.md`（W-1 以 `$3 == " partial "` 定位 `merge` 主行原地更新，`:205` 附录行不改）。文档关键字（EN/zh 命令页）：`destdir~HEAD`。
**Rollback mode:** immutable-release
**Estimated scope:** M（2 个落点，2 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=merge的D/F记录destdir~HEADstage1/2; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CSR-30,DEP-CSR-02,DEP-CSR-04,DEP-CSR-07,DEP-CSR-09; writeset=序列化于CSR-30; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-22: 修复 `libra switch -C <branch>` 在 unborn HEAD 上执行成功（#1030）

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 解决 Issue #1030：仓库尚无提交时，`libra switch -C <branch>`（无 `<start-point>`）把 unborn `HEAD` 改指向新分支名并保留索引与工作区，使 `t3404` 等初始仓库脚本可建立分支；有提交时行为不变。
**Out of scope:** 显式 `<start-point>` 无效时的报错（保持 fail-closed）。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `-C` 分支在无提交时解析 `HEAD` 失败 | `src/command/switch.rs:980` |
**Acceptance criteria:**
- [ ] **AC1** 刚 `libra init` 的空仓库上 `libra switch -C topic` 退出 0。
- [ ] **AC2** 上述命令后 `HEAD` 指向 unborn `refs/heads/topic`。
- [ ] **AC3** 随后首次 `libra commit` 的提交位于 `topic`。
- [ ] **AC4** 有提交的仓库上 `libra switch -C ex HEAD~1` 把 `refs/heads/ex` 重置到 `HEAD~1`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–8 步逐文件交付并验收（不计入卡特有 AC 分子）。
**AC → 断言映射:**（夹具 F22：AC1–AC3 为刚 `libra init` 的空仓库，已 `libra add` 暂存 `a.txt`=`A\n`（索引非空），另有未跟踪 `b.txt`=`B\n`；AC1–AC3 为同一仓库上的连续调用；AC4 为 `main` 上 C1→C2、分支 `ex` 指向 C2 的仓库；Git 期望按「验证执行协议」第 4 项取自同构 Git 夹具，快照按第 6 项）
- AC1 → V1：exit 0（Git 期望同为 0）。
- AC2 → V1：`HEAD` 符号指向 `refs/heads/topic`，`reference` 表无任何 `refs/heads/` 行；`S.wt` 与 `S.idx` 不变。
- AC3 → V1：随后 `libra commit -m c1` 后 `libra rev-parse refs/heads/topic` 等于 `libra rev-parse HEAD`，且 `reference` 表无 `refs/heads/main` 行。
- AC4 → V1：exit 0；`refs/heads/ex` 等于 C1（等于 Git 期望）。
- 文档 AC → 「文档与网站验收命令」第 1–8 步（不计入卡特有 AC 分子）。
**Verification:**
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::switch_test::switch_force_create_on_unborn_head)'`（卡特有 `(new)`；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项）
- [ ] 共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::switch_test::/)'`（按「验证执行协议」第 5 项；不计入 VER）
- [ ] 共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::switch::/)'`（按「验证执行协议」第 5 项；不计入 VER）
**Full-suite trigger:** T-1：本卡 C 组第②步 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13 `T-1`），并修改共享切换模块 `src/command/switch.rs`；C 组第④步在 bump 后的树上跑双环境完整 nextest，执行阶段跑本卡 A 组 focused 门
**Dependencies:** CSR-21, DEP-CSR-02, DEP-CSR-07, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `src/command/switch.rs`, `tests/command/switch_test.rs`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** 逐文件更新 `docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`（unborn 仓库上 `-C`）、网站 `switch.en.md`、`COMPATIBILITY.md`（W-1 定位 `switch` 行原地更新）。文档关键字（EN/zh 命令页）：`switch -C`。
**Rollback mode:** immutable-release
**Estimated scope:** M（1 个落点，1 个生产文件）
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=switch-C在unbornHEAD; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CSR-21,DEP-CSR-02,DEP-CSR-07,DEP-CSR-09; writeset=序列化于CSR-21; release=independent; split-from=N/A; exception=N/A`

---

### Task CSR-31: `REL-CSR-03` (`checkout` 追踪状态输出与抑制) 家族发布点

**Task type:** release
**Lifecycle / Acceptance:** pending / 空
**Description:** 聚合 `CSR-08`, `CSR-28`（`REL-CSR-03`），在全部子卡本地签名提交并通过本地 A/B、ER-05 与 fmt/clippy 后，统一执行 C 组（`patch + 1`、版本面三处同步、工具链刷新 `Cargo.lock`、T-2 全量、签名提交、PR 与 squash merge、`gh release create`）、D 组与家族网站第 5–7 步，作为该家族唯一发布点（G-08）。
**Out of scope:** 不引入新产品代码或文档内容改动。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CSR-08`, `CSR-28` 同属一个公开行为面，须同版发布 | 「发布分组与并发窗口」`REL-CSR-03` |
**Acceptance criteria:**
- [ ] **AC1** `CSR-08` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC2** `CSR-28` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC3** 聚合守卫：家族全部卡特有用例（下方「聚合守卫」所列）在已 bump 的发布树上逐条按「验证执行协议」第 1–3 项通过。
- [ ] **AC4** `gh release view v<version>` 的 release note 正文逐条列出 `CSR-08` 与 `CSR-28` 的用户可见变化。
- [ ] **AC5** 每张子卡在「文档与网站验收命令」第 1 步登记的每个仓库文档改动后 SHA-256，等于该子卡本地签名提交中同一文件的 SHA-256。
- [ ] **AC6** 每个被家族子卡修改的仓库文档在发布树中的 SHA-256，等于家族顺序中最后修改它的子卡的登记值。
- [ ] **AC7** 家族网站页 `checkout.en.md` 与 `status.en.md` 按「文档与网站验收命令」第 2–7 步完成（第 3 步以本卡合入 `main` 后的家族最终 EN 文档同步，第 8 项家族口径）。
（ER-04 的 fmt / clippy / T-2 全量与 ER-08 版本面门按 G-03 不计入本卡特有 AC/VER 分子。）
**AC → 断言映射:**（非测试型判据按「验证执行协议」第 9 项；`<REL>` 为本卡 C 组第⑦步签名提交 SHA，`<子卡 提交>` 为各子卡本地签名提交 SHA（登记于子卡记录））
- AC1 → V1：`libra merge-base --is-ancestor <CSR-08 提交> <REL>` exit 0。
- AC2 → V2：`libra merge-base --is-ancestor <CSR-28 提交> <REL>` exit 0。
- AC3 → 聚合守卫：所列每条命令按协议第 1–3 项判定全部通过（ER-04 B 组 `release` 类型门），逐条记录 exit code 与计数。
- AC4 → V3：`gh release view v<version> --json body --jq .body` exit 0，正文对 `CSR-08` 与 `CSR-28` 各有一条变化说明（逐条以卡 ID 标注）。
- AC5 → V4：对每张子卡登记的每个文档执行 `libra show <该子卡提交>:<路径> | shasum -a 256`，逐一等于其登记值；任一不等即失败。
- AC6 → V5：对每个文档取最后修改它的子卡登记值，与 `libra show <REL>:<路径> | shasum -a 256` 比较相等；任一不等即失败。
- AC7 → V6：第 2–7 步逐步记录命令、退出码与 SHA-256；任一步 STOP 即不成立。
**Verification:**
- [ ] V1 `libra merge-base --is-ancestor <CSR-08 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V2 `libra merge-base --is-ancestor <CSR-28 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V3 `gh release view v<version> --json body --jq .body`（非测试型，按协议第 9 项；exit 0 且逐卡说明齐全为通过）
- [ ] V4 `libra show <子卡提交>:<路径> | shasum -a 256`（逐子卡逐文件；非测试型，按协议第 9 项）
- [ ] V5 `libra show <REL>:<路径> | shasum -a 256`（逐文件；非测试型，按协议第 9 项）
- [ ] V6 「文档与网站验收命令」第 2–7 步（非测试型，按协议第 9 项；各步判据见该节）
**聚合守卫（ER-04 B 组 `release` 类型门，不计入 VER）：** `source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_prints_tracking_status)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_tracking_status_suppression_and_json)'`
**Full-suite trigger:** T-2：`family release point`（C 组第④步在 bump 后的精确提交树上跑 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 与 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，记录提交 SHA 与 `HEAD^{tree}`；合入与发布前核对发布树与已测树一致，变化即重跑）
**Dependencies:** CSR-28, DEP-CSR-02, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `docs/development/plan/plan-20261117-checkout-switch-restore.md`（发布证据登记）, `tests/INDEX.md`（仅需同步时）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`（第 6 步提交）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 版本面三处（`Cargo.toml`、`install.sh`、`install.ps1`）与工具链刷新的 `Cargo.lock`（属 `Release write set`）
**Docs and compatibility impact:** 不新增文档内容：汇总子卡文档交付证据（AC5、AC6），以家族最终 EN 文档完成网站 `checkout.en.md` 与 `status.en.md` 第 2–7 步（AC7）。
**Rollback mode:** immutable-release
**Estimated scope:** S（0 个生产文件，仅家族发布聚合）
**Version increment:** patch
**Release boundary:** family release point of REL-CSR-03
**C/D coverage from:** self
**Granularity:** `type=release; axis=REL-CSR-03家族发布点; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=7/12; VER=6/12; landing=0; prod-files=0; scope=S; deps=CSR-28,DEP-CSR-02,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-28; release=family release point; split-from=N/A; exception=N/A`

---

### Task CSR-32: `REL-CSR-04` (`checkout -p` 与 `overlay` 默认/互斥) 家族发布点

**Task type:** release
**Lifecycle / Acceptance:** pending / 空
**Description:** 聚合 `CSR-02`, `CSR-27`, `CSR-27a`, `CSR-03`（`REL-CSR-04`），在全部子卡本地签名提交并通过本地 A/B、ER-05 与 fmt/clippy 后，统一执行 C 组（`patch + 1`、版本面三处同步、工具链刷新 `Cargo.lock`、T-2 全量、签名提交、PR 与 squash merge、`gh release create`）、D 组与家族网站第 5–7 步，作为该家族唯一发布点（G-08）。
**Out of scope:** 不引入新产品代码或文档内容改动。
**Current evidence:**
| 事实 | 证据 |
|---|---|
| `CSR-02`, `CSR-27`, `CSR-27a`, `CSR-03` 同属一个公开行为面，须同版发布 | 「发布分组与并发窗口」`REL-CSR-04` |
**Acceptance criteria:**
- [ ] **AC1** `CSR-02` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC2** `CSR-27` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC3** `CSR-27a` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC4** `CSR-03` 的本地签名提交是本卡发布提交（C 组第⑦步签名提交）的祖先。
- [ ] **AC5** 聚合守卫：家族全部卡特有用例（下方「聚合守卫」所列）在已 bump 的发布树上逐条按「验证执行协议」第 1–3 项通过。
- [ ] **AC6** `gh release view v<version>` 的 release note 正文逐条列出 `CSR-02`、`CSR-27`、`CSR-27a` 与 `CSR-03` 的用户可见变化。
- [ ] **AC7** 每张子卡在「文档与网站验收命令」第 1 步登记的每个仓库文档改动后 SHA-256，等于该子卡本地签名提交中同一文件的 SHA-256。
- [ ] **AC8** 每个被家族子卡修改的仓库文档在发布树中的 SHA-256，等于家族顺序中最后修改它的子卡的登记值。
- [ ] **AC9** 家族网站页 `checkout.en.md` 按「文档与网站验收命令」第 2–7 步完成（第 3 步以本卡合入 `main` 后的家族最终 EN 文档同步，第 8 项家族口径）。
（ER-04 的 fmt / clippy / T-2 全量与 ER-08 版本面门按 G-03 不计入本卡特有 AC/VER 分子。）
**AC → 断言映射:**（非测试型判据按「验证执行协议」第 9 项；`<REL>` 为本卡 C 组第⑦步签名提交 SHA，`<子卡 提交>` 为各子卡本地签名提交 SHA（登记于子卡记录））
- AC1 → V1：`libra merge-base --is-ancestor <CSR-02 提交> <REL>` exit 0。
- AC2 → V2：`libra merge-base --is-ancestor <CSR-27 提交> <REL>` exit 0。
- AC3 → V3：`libra merge-base --is-ancestor <CSR-27a 提交> <REL>` exit 0。
- AC4 → V4：`libra merge-base --is-ancestor <CSR-03 提交> <REL>` exit 0。
- AC5 → 聚合守卫：所列每条命令按协议第 1–3 项判定全部通过（ER-04 B 组 `release` 类型门），逐条记录 exit code 与计数。
- AC6 → V5：`gh release view v<version> --json body --jq .body` exit 0，正文对 `CSR-02`、`CSR-27`、`CSR-27a` 与 `CSR-03` 各有一条变化说明（逐条以卡 ID 标注）。
- AC7 → V6：对每张子卡登记的每个文档执行 `libra show <该子卡提交>:<路径> | shasum -a 256`，逐一等于其登记值；任一不等即失败。
- AC8 → V7：对每个文档取最后修改它的子卡登记值，与 `libra show <REL>:<路径> | shasum -a 256` 比较相等；任一不等即失败。
- AC9 → V8：第 2–7 步逐步记录命令、退出码与 SHA-256；任一步 STOP 即不成立。
**Verification:**
- [ ] V1 `libra merge-base --is-ancestor <CSR-02 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V2 `libra merge-base --is-ancestor <CSR-27 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V3 `libra merge-base --is-ancestor <CSR-27a 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V4 `libra merge-base --is-ancestor <CSR-03 提交> <REL>`（非测试型，按「验证执行协议」第 9 项；exit 0 为通过，1 或 >1 均失败）
- [ ] V5 `gh release view v<version> --json body --jq .body`（非测试型，按协议第 9 项；exit 0 且逐卡说明齐全为通过）
- [ ] V6 `libra show <子卡提交>:<路径> | shasum -a 256`（逐子卡逐文件；非测试型，按协议第 9 项）
- [ ] V7 `libra show <REL>:<路径> | shasum -a 256`（逐文件；非测试型，按协议第 9 项）
- [ ] V8 「文档与网站验收命令」第 2–7 步（非测试型，按协议第 9 项；各步判据见该节）
**聚合守卫（ER-04 B 组 `release` 类型门，不计入 VER）：** `source .env.test && cargo nextest run --lib -E 'test(=cli::tests::declined_interactive_table_omits_checkout_patch)'`；`source .env.test && cargo nextest run --lib -E 'test(=cli::tests::declined_interactive_table_omits_checkout_auto_advance)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_patch_mode)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_patch_rejects_branch_creation)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_patch_auto_advance_behavior)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_auto_advance_requires_patch)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_patch_hunk_context)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_patch_selected_hunk_preserves_other)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_patch_context_requires_patch)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_overlay_default_and_toggle)'`；`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_overlay_rejections)'`；共享回归组 `source .env.test && cargo nextest run --test compat_declined_interactive_flags`
**Full-suite trigger:** T-2：`family release point`（C 组第④步在 bump 后的精确提交树上跑 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 与 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，记录提交 SHA 与 `HEAD^{tree}`；合入与发布前核对发布树与已测树一致，变化即重跑）
**Dependencies:** CSR-03, DEP-CSR-02, DEP-CSR-03, DEP-CSR-07, DEP-CSR-08, DEP-CSR-09
**Deliverables:** N/A
**Implementation write set:** `docs/development/plan/plan-20261117-checkout-switch-restore.md`（发布证据登记）, `tests/INDEX.md`（仅需同步时）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`（第 6 步提交）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（「文档与网站验收命令」第 4 步重生成的被忽略产物，不提交）
**Release write set:** Inherited
**Files likely touched:** 版本面三处（`Cargo.toml`、`install.sh`、`install.ps1`）与工具链刷新的 `Cargo.lock`（属 `Release write set`）
**Docs and compatibility impact:** 不新增文档内容：汇总子卡文档交付证据（AC7、AC8），以家族最终 EN 文档完成网站 `checkout.en.md` 第 2–7 步（AC9）。
**Rollback mode:** immutable-release
**Estimated scope:** S（0 个生产文件，仅家族发布聚合）
**Version increment:** patch
**Release boundary:** family release point of REL-CSR-04
**C/D coverage from:** self
**Granularity:** `type=release; axis=REL-CSR-04家族发布点; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=9/12; VER=8/12; landing=0; prod-files=0; scope=S; deps=CSR-03,DEP-CSR-02,DEP-CSR-03,DEP-CSR-07,DEP-CSR-08,DEP-CSR-09; writeset=序列化于CSR-03; release=family release point; split-from=N/A; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `src/cli.rs` D15 表解禁（`CSR-01`/`CSR-02`/`CSR-27`） | 各卡 `cli::tests::*` 卡特有用例；共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::/)'` |
| 集成 | 20 个 Issue 的 CLI 行为、patch 模式、`-m` 合并、`merge` D/F stage 1/2 与 unborn `-C` | 各卡 `Verification` 所列 `--test command_test -E 'test(=…)'` 卡特有用例；共享回归组按模块各跑一条 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::<模块>::/)'`，`<模块>` 依次为 `checkout_test`、`switch_test`、`restore_test`、`merge_test`、`status_test`、`add_patch_test` |
| 兼容 | 版本面同步与 D15 拒绝表守卫 | 共享回归组 `source .env.test && cargo nextest run --test compat_version_surface_sync`；共享回归组 `source .env.test && cargo nextest run --test compat_declined_interactive_flags` |
| 迁移 | N/A：无 SQLite schema 迁移 | N/A |
| 安全 | 符号链接不跟随删除（`CSR-11`）、前导目录符号链接保护（`CSR-14`）、普通 `checkout HEAD` 不删用户文件（`CSR-13`） | 上述三卡的卡特有用例（见各卡 `Verification`） |
| 性能 | 继承 GC-10，索引与路径遍历无无界扫描 | N/A |
| live/gated | 计划收口双环境全量门 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| CSR-01 | Issue #1037 部分 1/2 / `src/internal/patch_mode/mod.rs:1-6` | `src/cli.rs`、`src/internal/patch_mode/`、`src/command/restore.rs` | `tests/compat/README.md`、`docs/commands/restore.md`、`docs/commands/zh-CN/restore.md`、`docs/development/commands/restore.md`、`docs/development/commands/_compatibility.md`、`restore.en.md`（网站）、`COMPATIBILITY.md` | `cli::tests::declined_interactive_table_omits_restore_patch`、`command::restore_test::restore_patch_mode`、`command::restore_test::restore_patch_rejects_pathspec_from_file` |
| CSR-23 | Issue #1037 部分 2/2 / `src/internal/patch_mode/mod.rs:1-6` | `src/cli.rs`、`src/internal/patch_mode/`、`src/command/restore.rs` | `docs/commands/restore.md`、`docs/commands/zh-CN/restore.md`、`docs/development/commands/restore.md`、`restore.en.md`（网站）、`COMPATIBILITY.md` | `command::restore_test::restore_patch_controls`、`command::restore_test::restore_patch_source` |
| CSR-25 | `REL-CSR-01` 家族发布点 | 版本面三处与 `Cargo.lock`（`Release write set`） | 汇总子卡文档证据；网站 `restore.en.md` | 聚合守卫（子卡全部卡特有用例） |
| CSR-02 | Issue #702 部分 1/4 / `src/cli.rs:3039-3048` | `src/cli.rs`、`src/internal/patch_mode/`、`src/command/checkout.rs` | `tests/compat/README.md`、`docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/development/commands/_compatibility.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `cli::tests::declined_interactive_table_omits_checkout_patch`、`command::checkout_test::checkout_patch_mode`、`command::checkout_test::checkout_patch_rejects_branch_creation` |
| CSR-27 | Issue #702 部分 2/4 / `src/cli.rs:3050-3070,5370` | `src/cli.rs`、`src/command/checkout.rs` | `tests/compat/README.md`、`docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/development/commands/_compatibility.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `cli::tests::declined_interactive_table_omits_checkout_auto_advance`、`command::checkout_test::checkout_patch_auto_advance_behavior`、`command::checkout_test::checkout_auto_advance_requires_patch` |
| CSR-27a | Issue #702 部分 3/4 / `src/command/reset.rs:535` | `src/command/checkout.rs`、`src/command/diff.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::checkout_patch_hunk_context`、`command::checkout_test::checkout_patch_selected_hunk_preserves_other`、`command::checkout_test::checkout_patch_context_requires_patch` |
| CSR-03 | Issue #702 部分 4/4 / `src/command/checkout.rs:100-107,706-722` | `src/command/checkout.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::checkout_overlay_default_and_toggle`、`command::checkout_test::checkout_overlay_rejections` |
| CSR-32 | `REL-CSR-04` 家族发布点 | 版本面三处与 `Cargo.lock`（`Release write set`） | 汇总子卡文档证据；网站 `checkout.en.md` | 聚合守卫（子卡全部卡特有用例） |
| CSR-04 | Issue #703 / `src/command/checkout.rs:53-120` | `src/command/checkout.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::checkout_ours_theirs_write_stage`、`command::checkout_test::checkout_ours_theirs_rejections` |
| CSR-05 | Issue #704 / `src/command/checkout.rs:706-757` | `src/command/checkout.rs`、`src/command/restore.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::no_overlay_tag_source`、`command::checkout_test::no_overlay_invalid_tag_rejected` |
| CSR-06 | Issue #705 / `src/command/checkout.rs:664-704` | `src/command/checkout.rs`、`src/command/restore.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::restore_deleted_with_untracked_present` |
| CSR-07 | Issue #706 / `src/command/checkout.rs:57-77` | `src/command/checkout.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::detach_incompatible_with_branch_create` |
| CSR-08 | Issue #707 部分 1/2 / `src/command/checkout.rs:1229-1286` | `src/command/checkout.rs`、`src/command/status.rs`、`src/command/status/output.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`checkout.en.md`（网站）、`status.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::checkout_prints_tracking_status` |
| CSR-28 | Issue #707 部分 2/2 / `src/command/checkout.rs:1229-1286` | `src/command/checkout.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::checkout_tracking_status_suppression_and_json` |
| CSR-31 | `REL-CSR-03` 家族发布点 | 版本面三处与 `Cargo.lock`（`Release write set`） | 汇总子卡文档证据；网站 `checkout.en.md`, `status.en.md` | 聚合守卫（子卡全部卡特有用例） |
| CSR-09 | Issue #708 部分 1/3 / `src/command/checkout.rs:53-120` | `src/command/checkout.rs`、`src/command/restore.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::pathspec_from_file_reads_lines`、`command::checkout_test::pathspec_from_file_incompatible_options_rejected` |
| CSR-33 | Issue #708 部分 2/3 / `src/command/restore.rs:383-390` | `src/command/checkout.rs`、`src/command/restore.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::pathspec_from_stdin` |
| CSR-24 | Issue #708 部分 3/3 / `src/command/checkout.rs:53-120` | `src/command/checkout.rs`、`src/command/restore.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::pathspec_file_nul` |
| CSR-34 | `REL-CSR-05` 家族发布点 | 版本面三处与 `Cargo.lock`（`Release write set`） | 汇总子卡文档证据；网站 `checkout.en.md` | 聚合守卫（子卡全部卡特有用例） |
| CSR-10 | Issue #709 / `src/command/checkout.rs:84-85,899-914` | `src/command/checkout.rs`、`src/command/branch.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::track_inherit_and_auto_setup_merge` |
| CSR-10a | Issue #709 / `src/command/checkout.rs:84-85` | `src/command/checkout.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::bare_track_matches_direct` |
| CSR-10R | `REL-CSR-TRACK` 家族发布点 | 版本面三处与 `Cargo.lock`（`Release write set`） | 汇总子卡文档证据；网站 `checkout.en.md` | 聚合守卫（子卡全部卡特有用例） |
| CSR-11 | Issue #712 / `src/command/restore.rs:684,764,2045-2056,2164,2206` | `src/command/checkout.rs`、`src/command/switch.rs`、`src/command/restore.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`checkout.en.md`（网站）、`switch.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::force_checkout_does_not_follow_leading_symlink`、`command::switch_test::force_switch_does_not_follow_leading_symlink` |
| CSR-12 | Issue #713 / `src/command/checkout.rs:53-120` | `src/command/checkout.rs`、`src/command/switch.rs`、`src/command/restore.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::merge_checkout_creates_conflict_stages`、`command::checkout_test::merge_checkout_failure_rolls_back` |
| CSR-13 | Issue #714 部分 1/2 / `src/command/status.rs:745` | `src/command/status.rs`、`src/command/checkout.rs`、`src/command/restore.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`docs/error-codes.md`、`checkout.en.md`（网站）、`status.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::tracked_dir_replaced_by_file_status_and_noop` |
| CSR-29 | Issue #714 部分 2/2 / Issue #714 | `src/command/checkout.rs`、`src/command/switch.rs`、`src/command/restore.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`checkout.en.md`（网站）、`switch.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::force_head_recovers_dir_and_ft`、`command::switch_test::force_switch_recovers_dir_and_ft` |
| CSR-14 | Issue #715 / `src/command/restore.rs:562,684,1235-1260,1817-1840` | `src/command/checkout.rs`、`src/command/switch.rs`、`src/command/restore.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`checkout.en.md`（网站）、`switch.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::force_replaces_leading_dir_symlink`、`command::switch_test::force_replaces_leading_dir_symlink` |
| CSR-15 | Issue #716 / `src/command/checkout.rs:461-470` | `src/command/checkout.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::orphan_with_start_point` |
| CSR-15a | Issue #716 / `src/command/checkout.rs:461-470` | `src/command/checkout.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::orphan_start_point_dirty_conflict_and_rollback` |
| CSR-15R | `REL-CSR-ORPHAN` 家族发布点 | 版本面三处与 `Cargo.lock`（`Release write set`） | 汇总子卡文档证据；网站 `checkout.en.md` | 聚合守卫（子卡全部卡特有用例） |
| CSR-16 | Issue #718 / `src/command/checkout.rs:960-969` | `src/command/checkout.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::checkout_b_from_unborn_orphan` |
| CSR-17 | Issue #721 / `src/command/checkout.rs:664-694` | `src/command/checkout.rs`、`src/command/switch.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`checkout.en.md`（网站）、`switch.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::dwim_ambiguous_remote_does_not_fallback_to_pathspec`、`command::switch_test::dwim_ambiguous_remote` |
| CSR-18 | Issue #723 / `src/command/switch.rs:852-867` | `src/command/checkout.rs`、`src/command/switch.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::force_branch_b_preserves_staged_mergeable_changes`、`command::switch_test::checkout_b_and_switch_c_conflict_preserves_state` |
| CSR-19 | Issue #724 / `src/command/checkout.rs:1002-1022` | `src/command/checkout.rs`、`src/command/switch.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`checkout.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::dwim_single_non_origin_remote` |
| CSR-20 | Issue #1008 部分 1/3 / `src/command/restore.rs:1601-1615,1933-1997` | `src/command/checkout.rs`、`src/command/switch.rs`、`src/command/restore.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`checkout.en.md`（网站）、`switch.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::replace_non_empty_tracked_dir_with_file`、`command::switch_test::replace_non_empty_tracked_dir_with_file` |
| CSR-30 | Issue #1008 部分 2/3 / `src/command/restore.rs:1601-1615,1933-1997` | `src/command/checkout.rs`、`src/command/switch.rs`、`src/command/restore.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`checkout.en.md`（网站）、`switch.en.md`（网站）、`COMPATIBILITY.md` | `command::checkout_test::force_replace_non_empty_dir_with_file`、`command::switch_test::force_replace_non_empty_dir_with_file` |
| CSR-21 | Issue #1008 部分 3/3 / `src/command/merge.rs:4031` | `src/command/merge.rs`、`src/command/merge/rename_merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`merge.en.md`（网站）、`COMPATIBILITY.md` | `command::merge_test::df_merge_records_stage_1_and_2`、`command::merge_test::modify_delete_conflict_still_reported` |
| CSR-22 | Issue #1030 / `src/command/switch.rs:980` | `src/command/switch.rs` | `docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`switch.en.md`（网站）、`COMPATIBILITY.md` | `command::switch_test::switch_force_create_on_unborn_head` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划文本双审 PASS（Claude 与 Codex 对同一字节），基线冻结（`62abb622...`，`0.30.207`） | Claude `VERDICT: PASS` + Codex `VERDICT: PASS` | N/A |
| M1 | Phase 0 活动卡（`CSR-01`..`CSR-09`、`CSR-23`..`CSR-25`、`CSR-27`、`CSR-27a`、`CSR-28`、`CSR-31`..`CSR-34`；不含已废弃的 `CSR-35`）完成且发布 | 4 个独立 patch release（`CSR-04`..`CSR-07`）+ 4 个家族发布点（`REL-CSR-01`/`03`/`04`/`05`）+ D 组全绿 | 按 `immutable-release` 前滚修复 |
| M2 | Phase 1 (`CSR-10`、`CSR-10a`、`CSR-10R`、`CSR-11`..`CSR-15`、`CSR-15a`、`CSR-15R`、`CSR-16`、`CSR-29`) 完成且 `T-1` 全量通过 | 独立卡与两个家族发布点 patch release + nextest 全绿 | 按 `immutable-release` 前滚修复 |
| M3 | Phase 2 (`CSR-17`..`CSR-22`、`CSR-30`) 及收口全量门全绿 | 全部 38 张活动卡 `done/complete` | 按 `immutable-release` 前滚修复 |

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
| `restore.rs` / `switch.rs` / `merge.rs` / `status.rs` / `cli.rs` 改动影响面广 | 中：可能影响其他命令 | 独立发布卡按卡内 `Full-suite trigger` 的 `T-1` 跑全量；家族子卡 `CSR-01`/`02`/`03`/`08`/`09`/`23`/`24`/`27`/`27a`/`28`/`33`、`CSR-10`/`10a` 和 `CSR-15`/`15a` 不独立发布，由各自发布点 `CSR-25`/`31`/`32`/`34`、`CSR-10R` 和 `CSR-15R` 的 `T-2` 覆盖；`CSR-35` 已废弃 | `CSR-05`, `CSR-06`, `CSR-10`, `CSR-10a`, `CSR-10R`, `CSR-11`..`CSR-15`, `CSR-15a`, `CSR-15R`, `CSR-16`..`CSR-22`, `CSR-27`, `CSR-27a`, `CSR-29`, `CSR-30`, `CSR-25`, `CSR-31`, `CSR-32`, `CSR-34` |
| 网站检出 `/Volumes/Data/gitmono/libra-backend`（`.libra`、`cf`）预检失败，或与其它计划争用同一网站页/生成产物 | 中：阻塞网站交付 | 「文档与网站验收命令」第 2 步预检与 `DEP-CSR-08`/`DEP-CSR-09` 串行窗口，遇阻 STOP 报告 | 全部含网站页的 `CSR-*` |
| 跨计划写锁等待（`PM-21a`、`GII-21`/`GII-23`、`DIFF-09`、`CS-08` 等）拉长家族窗口 | 中：进度延后 | 家族首卡开工前一次性核对 `DEP-CSR-03`/`08`/`09`/`12`，未满足不开工，避免持锁等待；超 7 日报告协调者 | `CSR-02`、`CSR-27a` 与全部家族卡 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 路径/D/F 冲突预检 | $O(N)$（$N$ 为索引或变更条目数） | 单次切换 1 次遍历 | 无全仓递归无界扫描 | `command::checkout_test` |
| 交互式 hunk 切分 | $O(\text{diff lines})$ | 按修改文件线性处理 | 单文件受大文件保护 | `command::restore_test` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已随各卡同步更新 `checkout`、`switch`、`restore`、`status`、`merge` 行。
- [ ] `docs/commands/{checkout,switch,restore,status,merge}.md` 与 `zh-CN/` 文档已同步。
- [ ] `docs/development/commands/{checkout,switch,restore,status,merge,_compatibility}.md` 已同步。
- [ ] `docs/error-codes.md` 已核对/同步。
- [ ] `tests/INDEX.md` 与 `Cargo.toml` `[[test]]` 已核对（复用既有 `--test command_test`）。
- [ ] `plan-long.md` 日期计划索引已同步或确认 `N/A`。

## Codex review log

Result 只允许 `PASS` 或 `FAIL`。`FAIL` 必须列出 P0/P1 条目并在下一轮复审关闭；P2 可由具名责任人书面接受为 residual risk，但不改变本轮 `FAIL` 记录（ER-05）。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
| R1（2026-11-18 双审；Codex gpt-6.1-terra，A–E 五分区） | 受审 SHA-256 `8dad1040f10e…` 全文 | FAIL | A：Git 侧 `S.wt` 未排除 `.git`；测试矩阵正则 `\|` 不可执行。B：家族发布点文档哈希门自相矛盾；`CSR-02` 未验收非 HEAD `<rev>`；`CSR-27` 交互与副作用门矛盾；`CSR-23` merge-base 夹具无区分力。C：同一哈希门；`CSR-08` 复用 status 渲染与 Git 逐字期望矛盾；`.git`。D：同一哈希门；`CSR-11` 未覆盖 detached 路径；`.git`；成功切换未声明 reflog 变化。E：`CSR-18` 未比较完整索引且计数低报；`CSR-19` 未验证不访问网络；`CSR-17` switch 未断言稳定码 | P2（`CSR-27` 两选项顺序、`CSR-27a` `-U0` 应用、`CSR-33` 守卫强度）：`CSR-33` 已加强，其余登记为残余风险 | Codex 只读评审输出（评审工作目录，不入库） |
| R1（2026-11-18 双审；Claude subagent claude-opus-5-5） | 受审 SHA-256 `8dad1040f10e…` 全文 | FAIL | `CSR-02` AC2 夹具三处修改合成一个 hunk；`CSR-14` 非 force 场景不写 `dir/file`；Git 期望构建改用只读 `git archive`；`CSR-12`/`16`/`19`/`22` 复合 AC；`CSR-06`/`13`/`29` 映射附加谓词；夹具复用未定义；`CSR-19` 误用「对照」 | P2 同上 | Claude 评审笔记（评审工作目录，不入库） |
| R2（2026-11-18 双审；Codex gpt-6.1-terra，A–E 五分区） | 受审 SHA-256 `323cf204dfa4…` 全文 | FAIL | A：引用快照未定义 `reference` 表短名口径。B：`CSR-01`/`CSR-02` 成功路径未保护另一侧（索引/工作区）；`CSR-03` 缺 `--no-overlay` 切换拒绝。C：路径文件引号解析；`CSR-08` 分叉段比较不完整；`CSR-06` 只查单文件；`CSR-07` 文案 AC。D：`CSR-14` 缺 `switch -f` 外部目录保护。E：`CSR-19` 默认远端夹具挂错 AC、「不访问远端」不可观测、追踪配置与副作用门冲突；`CSR-21` 冲突状态与失败快照门冲突 | 引号解析按 #708 验收范围登记 `DEFER-CSR-04`（驳回为 P1 的部分理由：不在 Issue 验收内）；P2：`CSR-33` 守卫强度登记残余风险 | Codex 只读评审输出（评审工作目录，不入库） |
| R2（2026-11-18 双审；Claude subagent claude-opus-5-5） | 受审 SHA-256 `323cf204dfa4…` 全文 | FAIL | 发布点 Dependencies 漏列其持有至合入的跨计划窗口（`DEP-CSR-03`/`05`/`06`）；另核对 Issue 验收发现 `CSR-09` 缺 #708 要求的诊断文案、`CSR-24` 缺 `--pathspec-file-nul` 文案、`CSR-18` 未按 #723 重置当前分支 | 无 | Claude 评审笔记（评审工作目录，不入库） |
| R3（2026-11-18 双审；Codex gpt-6.1-terra，A–E 五分区） | 受审 SHA-256 `490681c33c90…` 全文 | FAIL（A 区 PASS） | B：`CSR-03` 夹具以 tag 为源而 tag 解析由后续 `CSR-05` 交付。C：`CSR-33` 零命中守卫可被其它读取 API 绕过。D：`CSR-12` AC6 未建立冲突前置；`CSR-13` 保留断言未比较完整快照。E：`CSR-18` 未断言 `HEAD` 仍附着当前分支；`CSR-22` 空索引夹具无法判定「保留索引」 | A 区 P2（收口清单漏 `status`）已修 | Codex 只读评审输出（评审工作目录，不入库） |
| R3（2026-11-18 双审；Claude subagent claude-opus-5-5） | 受审 SHA-256 `490681c33c90…` 全文 | FAIL | 发布点文档哈希 AC 以「且」连接两个可分别失败的检查（G-03 低报） | P2：`CSR-16`/`CSR-22` 映射附带的保护性「不变」断言保留 | Claude 评审笔记（评审工作目录，不入库） |
| R4（2026-11-18 双审；Codex gpt-6.1-terra，A–E 五分区） | 受审 SHA-256 `82f0b6f34021…` 全文 | FAIL（B 区 PASS） | A/C：家族网站预检只认最后一张子卡登记值，`CSR-31` 的 `status.en.md` 无法交付；`DEP-CSR-07` 未允许 `status` 行。D：`CSR-15` AC4 未建立 orphan 前置；`CSR-29` 未断言切换到 `t`；`CSR-29` 根因证据与源码不符。E：`CSR-20` 成功场景未比较完整工作区；`CSR-19`「不访问远端」不可观测 | 无 | Codex 只读评审输出（评审工作目录，不入库） |
| R4（2026-11-18 双审；Claude subagent claude-opus-5-5） | 受审 SHA-256 `82f0b6f34021…` 全文 | FAIL | `CSR-10a` AC2 复合谓词（新建切换 + 上游配置） | P2：「发布窗口顺序」把家族子卡列为发版单元（已改） | Claude 评审笔记（评审工作目录，不入库） |
| R5（2026-11-18 双审；Codex gpt-6.1-terra，A–E 五分区） | 受审 SHA-256 `999b093fcaa7…` 全文 | FAIL（B、D 区 PASS） | A：`S.idx` 只取 `ls-files --stage`，漏 `skip-worktree` 等标志。C：`CSR-08` 写集漏 `src/command/status.rs`（`mod output` 私有）。E：`CSR-20` 成功场景未断言 `HEAD`；`CSR-30` 未断言索引与 `HEAD` | 无 | Codex 只读评审输出（评审工作目录，不入库） |
| R5（2026-11-18 双审；Claude subagent claude-opus-5-5） | 受审 SHA-256 `999b093fcaa7…` 全文 | FAIL | 切换类成功场景缺 `HEAD` 与完整结果状态断言：`CSR-11`、`CSR-14`（另删 #715 未要求场景）、`CSR-19`、`CSR-20`、`CSR-30`；`CSR-12` AC2 只查单文件 | 无 | Claude 评审笔记（评审工作目录，不入库） |
| R6（2026-11-18 双审；Codex gpt-6.1-terra，A–E 五分区） | 受审 SHA-256 `f7075bda1ea3…` 全文 | FAIL（A、B 区 PASS） | C：`CSR-28` 抑制场景未断言切换完成；`CSR-07` detach 夹具 `main` 与 `topic` 同提交无区分力；`CSR-04` 未验收 `-2`/`-3`。D：`CSR-29` 描述要求放宽预检，与根因证据矛盾。E：`CSR-19` 夹具未规定切换前后树不同 | 无 | Codex 只读评审输出（评审工作目录，不入库） |
| R6（2026-11-18 双审；Claude subagent claude-opus-5-5） | 受审 SHA-256 `f7075bda1ea3…` 全文 | PASS | 无 | P2：`DEFER-CSR-02` 延后上游整脚本回归而部分 Issue 验收列有该项（关闭口径依赖「成功定义」）；`CSR-21` 按 Git 在 stdout 断言 `CONFLICT` 行 | Claude 评审笔记（评审工作目录，不入库） |
| R7（2026-11-18 双审；Codex gpt-6.1-terra，A–E 五分区） | 受审 SHA-256 `1ca93d383ed3…` 全文 | FAIL（A、B 区 PASS） | C：F08 未写明各提交的树与操作顺序，夹具不可重建。D：`CSR-14` 成功场景未比较索引。E：`CSR-20` 原卡未按 G-09 登记拆出 `CSR-30`（同类问题全卡清扫） | 无 | Codex 只读评审输出（评审工作目录，不入库） |
| R7（2026-11-18 双审；Claude subagent claude-opus-5-5） | 受审 SHA-256 `1ca93d383ed3…` 全文 | PASS | 无 | P2：`CSR-04` Description 仍列 `-B` 而 AC 只覆盖 `-b`（Out of scope 已注明同一 Git 拒绝分支） | Claude 评审笔记（评审工作目录，不入库） |
| R8（2026-11-18 双审；Codex gpt-6.1-terra，A–E 五分区） | 受审 SHA-256 `96e19f32ff81…` 全文 | FAIL（A、E 区 PASS） | B：`CSR-27a` hunk 头只取行首 `@@`，管道 stdin 下第二个头接在提示后漏取。C：`CSR-08` 各场景与 `CSR-28` 缺上游场景只断言输出/`HEAD`，未比较切换后的工作区与索引。D：`CSR-10`/`CSR-10a` 成功场景只断言配置，空操作亦可通过 | 无 | Codex 只读评审输出（评审工作目录，不入库） |
| R8（2026-11-18 双审；Claude subagent claude-opus-5-5） | 受审 SHA-256 `96e19f32ff81…` 全文 | PASS | 无 | P2：`CSR-17` AC1「拒绝且不回退」措辞（已改） | Claude 评审笔记（评审工作目录，不入库） |
| R9（2026-11-18 双审；Codex gpt-6.1-terra，A–E 五分区） | 受审 SHA-256 `70f2daed4e6b…` 全文 | FAIL（A、C、D、E 区 PASS） | B：`CSR-27` 全答 `n`，测不出 `--no-auto-advance` 会话结束时丢弃所选 hunk 的实现 | C 区 P2（`CSR-06` 子目录 `.`）已补 | Codex 只读评审输出（评审工作目录，不入库） |
| R9（2026-11-18 双审；Claude subagent claude-opus-5-5） | 受审 SHA-256 `70f2daed4e6b…` 全文 | PASS | 无 | P2：切换结果门对 unborn `HEAD` 的「所指提交」表述隐含两侧均无提交 | Claude 评审笔记（评审工作目录，不入库） |
| R10（2026-11-18 双审；Codex gpt-6.1-terra，A–E 五分区） | 受审 SHA-256 `5b08ed22500f…` 全文 | FAIL（A、B、C、E 区 PASS） | D：`CSR-11` 修复点 `restore.rs:2206` 不在两个入口的实际删除链上（实际为 `:764` → `:2045-2056`）；`CSR-14` 把只用于显式目标的 `:1254-1260` 当作普通切换的保护 | B 区 P2（`CSR-27` 选项顺序、`CSR-27a` `-U0` 应用）已补 | Codex 只读评审输出（评审工作目录，不入库） |
| R10（2026-11-18 双审；Claude subagent claude-opus-5-5） | 受审 SHA-256 `5b08ed22500f…` 全文 | FAIL | 六处渲染残留 `\"`，`CSR-23` 的 `e` 编辑器脚本不可直接执行 | 无 | Claude 评审笔记（评审工作目录，不入库） |
| R11（2026-11-18 双审；Codex gpt-6.1-terra，A–E 五分区） | 受审 SHA-256 `fd3f96ccd5475143abafcee9cb2712e198a7095a3e2b07826d20ce395dd913e4` 全文 | PASS | 无（A–E 五分区均 PASS；上一轮 D 区两项 P1 与 Claude P1 已关闭） | P2：`CSR-27` 可补反向顺序 `--auto-advance --no-auto-advance`，登记为残余风险 | Codex 只读评审输出（评审工作目录，不入库） |
| R11（2026-11-18 双审；Claude subagent claude-opus-5-5） | 受审 SHA-256 `fd3f96ccd5475143abafcee9cb2712e198a7095a3e2b07826d20ce395dd913e4` 全文 | PASS | 无 | P2：`CSR-14` 证据 `:562` 应为 `:561-567`，登记为残余风险 | Claude 评审笔记（评审工作目录，不入库） |
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
| R19 (Codex gpt-6.1-terra) | `plan-20261117-checkout-switch-restore.md` | FAIL | Codex R19 `FAIL` P1: `发布分组与并发窗口` 仍残留 `REL-CSR-06`/`CSR-35`/`CSR-36`；`DEP-CSR-02` 漏 `/Volumes/Data/gitmono/libra-backend` 挂载优先核对；`CSR-11` 合并未跟踪与索引致实 `10/8`（Claude R19 `ECONNRESET`） | R20 全部同步关闭 | Codex R19 只读评审 |
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
| R62-A/R63-B (Codex gpt-6.1-terra) | 网站路径修订后的第 1–900 行及 CSR-27 | FAIL | R60-A 豁免批准证据不存在、两个 cli::tests 门误用 command_test、VER/审计列错位 | R66-A 改用 --lib、重记 VER=11/8、修正列并以新批准记录取代不可核验 R60-A | 原始输出 8358248c、a6f81e83；受审 SHA 7adb80fb |
| R64-C/R65-D (Codex gpt-6.1-terra) | 第 901–1802 行及 CSR-34/CSR-10/CSR-15 | FAIL | 裸 --track 兼容、orphan dirty/失败原子性、R60-A 证据与后续日志缺失 | R66-A 补验收/回归、豁免与后续日志，不以旧 R48-B 覆盖新字节 | 原始输出 cb60f058、21bf0f22；受审 SHA 7adb80fb |
| R75-A/R76-B (Codex gpt-6.1-terra) | 第 1–450 行与第 451–900 行，CSR-27 计数及豁免 | FAIL | R75-A 指出三个额外验证门、R74-A 证据与 CSR-15 故障谓词；R76-B 再次确认 VER 漏计 | R78-A 统一为 10 AC/14 VER 并补批准记录；后续 CSR-27 修订须重审 | 原始输出 3b912521、b841becb；受审 SHA 33d8ef148 与此前 8b648c417 |
| R79-A/R80-D (Codex gpt-6.1-terra) | 第 1–450 行与第 1351 行至末尾，CSR-34/批准记录 | FAIL | R79-A 指出审计镜像计数未同步；R80-D 指出 R75/R76 真实记录缺失 | 已同步 CSR-27 VER14、CSR-15 AC11，并补本行及 R75/R76 证据；不推定历史通过 | 原始输出 8994c159、003c66f5；受审 SHA 22fcfa6e |
## 修订与批准记录（非 review log）

| 记录 | 范围 | 类型 | 内容 | 后续验证 |
|---|---|---|---|---|
| R66-A | CSR-27 与 CSR-10 | 修订批准 | Eli 批准当时的 EX-CSR-27-PATCH 与 EX-CSR-10-TRACK-COMPAT；后续 CSR-27 范围变化由 R78-A 重新批准 | R67+ 分区复审 |
| R72-A | CSR-15 | 修订批准 | Eli 批准 EX-CSR-15-ORPHAN-STATE，以三种 dirty 冲突及共同回滚门为证据；R69-C 无可核验记录，不再引用 | R73+ 分区复审 |
| R74-A | CSR-27 夹具计数修订 | 历史过渡记录 | 曾将夹具移出 AC；该记录已被 R78-A 的 10 AC/14 VER 新批准取代，不作为当前批准依据 | R75/R76 复审后重新批准 |
| R78-A | CSR-27 拆分前历史范围 | 历史修订批准 | 当时记录的 10 AC/14 VER 仅适用于拆分前文本；现已拆为 CSR-27 与 CSR-27a，不作为当前卡的计数、豁免或批准依据 | 当前拆卡须重新完成分区复审 |

## 实施证据汇总

- 计划成稿时间：`2026-11-17 03:03:00 UTC`；最新修订时间：`2026-11-18 18:39:35 UTC`；仓库基线 `main@62abb622f0eb2626934633ada78061963ebf1f44`；`Cargo.toml` 版本 `0.30.207`。
- 后续各卡的 focused 测试计数、nextest 全量摘要、`release.yml` run ID 与 D 组结果将直接登记于本节。

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-CSR-01 | 无（20 个分配 issue 已全部进入 38 张活动卡；`CSR-35` 已废弃） | N/A | N/A | N/A |
| DEFER-CSR-02 | `#705` delayed-filter 进度输出、`#714`/`#715` parallel-checkout worker 计数、以及外部上游完整 `t*.sh` / `TC-xxxx` 脚本全量跑通 | 依赖尚未落地的并行检出/过滤子系统与外部全局 CLI 前置 | 上游并行检出/过滤引擎合入后，由 CLI / Git-compat 维护者执行完整上游回归 | 后续并行检出与上游兼容性收敛计划（Owner: 仓库 CLI 维护者） |
| DEFER-CSR-03 | `plan-20261117-fetch-push.md` 的 `DEP-FP-03` 移交的 `#719` 验收 3 / `#676`：`checkout -b new origin` 未出生分支检出 | 不在本计划跟踪的 20 个 issue 内（`DEP-CSR-11`），本计划无卡交付 | 用户或协调者指定本计划承接时，先修订计划新增卡并重新双审 | 未承接；按 `DEP-FP-03` 回落口径 `#719` 保持 OPEN |
| DEFER-CSR-04 | `--pathspec-from-file` 换行模式下 C 风格引号路径（`"…"` 转义）的解析，与 `restore` 共享的读取器 `read_restore_pathspec_file`（`src/command/restore.rs:383-410`）同样未实现 | 不在 #708 验收 1–6 内（其上游门为 `t/t2026-checkout-pathspec-file.sh#11` 的互斥诊断）；需同时改动 `restore` 的既有行为 | 用户或协调者要求 `restore`/`checkout` 支持引号路径时，以独立卡改共享读取器并补双命令回归 | 未承接；#708 按其验收 1–6 由 `CSR-09`/`CSR-33`/`CSR-24` 闭环 |

## 完成判据

- [ ] 全量收口门须在**最终代码树**上执行，并记录 `libra rev-parse HEAD` 与 `libra rev-parse HEAD^{tree}` 及结果；树一旦变化即须重跑，与是否发生修复无关。
- [ ] 计划文本经 Claude（Subagent）与 Codex 对同一字节双审，均返回字面 `VERDICT: PASS` 且 P0/P1 全部关闭；各卡网站交付另需独立 Claude Code `VERDICT: PASS`（「文档与网站验收命令」第 5 步），二者互不替代。
- [ ] 网站交付门：每张触及用户可见行为的卡均在其发布点完成 `/Volumes/Data/gitmono/libra-backend` 的最终字节同步检查、前端验证，并取得独立 Claude Code `PASS`，随后精确暂存、签名+DCO 提交、核对提交树与已审字节一致、推送 `cf` 并核验部署；计划文本双审不替代该网站门，任一环节失败即 STOP 且不得标 `complete`。
计划只有在以下条件全部满足后才能标记完成：

- [ ] 所有任务卡满足粒度规则 `G-*`：无未登记的 L 例外、无 XL 卡、无碎片卡、无未登记的合并发布例外、实现写集冲突均已消解；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务的 acceptance criteria 已满足，且 `Lifecycle=done` **且** `Acceptance=complete`（ER-04）。
- [ ] 所有任务的 Verification 命令已运行并记录结果。
- [ ] **计划完成门 / 全量收口门（ER-13）**：在全部任务卡完成后运行一次 —— `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿。
- [ ] **全量收口门暴露的 Bug 已全部处理（ER-13）**。
- [ ] 必要的 docs/compat/error-code/test-index 更新已完成。
- [ ] 每张卡的 `Rollback mode` 均已验证或记录。
- [ ] 执行期各卡 ER-05 review 均为 `PASS`，P0/P1 全部关闭。
- [ ] 每张独立发布卡与家族发布点（`CSR-25`、`CSR-31`、`CSR-32`、`CSR-34`、`CSR-10R`、`CSR-15R`）均已完成 `patch + 1`、版本面三处同步、工具链刷新 `Cargo.lock`、`gh release create v<version>` 触发 `release.yml` 并取得 Cloudflare 发布证据（ER-08）。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
- [ ] `plan-long.md` 相关状态或日期计划索引已同步，或明确 `N/A`。
