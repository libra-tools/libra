# `describe` 命令 Git 兼容性修复计划（2026-11-17）

> 模板版本：`v2.16`；继承 [plan-template.md](plan-template.md) 的 `GC-01..13`、`ER-01..14`、`G-01..11`。
> 状态：计划修订中（Codex R45-A/R46-A FAIL，tag 命名空间已拆卡，待重新复审）；全部任务 `pending / 空`，尚未开工、尚未修改代码、尚未发布版本。按用户 2026-11-17 指令，计划提交门为 Codex 单审，不再要求 Claude Review；网站交付仍按仓库工作区规则独立取得 Claude Code PASS。
> 源码基线：`62abb622f0eb2626934633ada78061963ebf1f44`；创建时间：`2026-11-17 02:52:00 UTC`；最近修订：`2026-11-17 12:27:28 UTC`。
> 覆盖 Open Issues：[#795](https://github.com/libra-tools/libra/issues/795)、[#796](https://github.com/libra-tools/libra/issues/796)、[#797](https://github.com/libra-tools/libra/issues/797)、[#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)、[#800](https://github.com/libra-tools/libra/issues/800)、[#802](https://github.com/libra-tools/libra/issues/802)、[#803](https://github.com/libra-tools/libra/issues/803)（共 8 个；另含 `DESC-09` 补齐 `rev-parse` 对 `<tag>-<N>-g<sha>` 的往返解析，以闭环 `#798`/`#799` 验收）。

## 文档职责

本文解决 `libra describe` 在 Git 兼容性测试（`t6120-describe.sh`）中暴露的 8 项选项解析、输出格式、过滤语义与错误路径缺陷，目标是逐卡交付可独立验收与发布的修复切片，使 `libra describe` 在 `--contains`、`--no-match`、`--all` + `--match`、misnamed annotated tag（`--abbrev=0`）、`--no-long`、`--no-abbrev`、`--dirty <commit>`、`--broken` 及 `rev-parse` 往返解析场景下与 Git 行为一致，并同步 `--json` 结构化输出与双语/网站文档。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收。

### 适用范围

- 命令与模块：`libra describe`（`src/command/describe.rs`、`src/command/describe_format.rs`、`src/command/describe_types.rs`、`src/command/describe_tests.rs`）及共享修订解析器（`src/utils/util.rs` 仅 `DESC-09`/`DESC-09b`/`DESC-09c`/`DESC-09a`/`DESC-10`/`DESC-10a..10k`）。
- 机器输出与错误码：`--json` 输出结构 `DescribeOutput`、`DescribeError` 与 `StableErrorCode`（`LBR-CLI-002`、`LBR-IO-001` 等）。
- 测试与文档：`tests/command/describe_test.rs`、`tests/command/describe_long_test.rs`（均归属 `--test command_test`）、`docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`、`COMPATIBILITY.md`（仅原地更新第 219 行，不增删行）。

### 非目标

- 不改变 `for-each-ref` 的 `%(describe)` atom 接口签名（仅保持 `describe_commit_for_atom` 兼容，新增 `DescribeArgs` 字段在 `describe.rs:159-173` 填默认值）。
- 不实现嵌套 tag（tag-of-tag）多层剥皮（由 `#498 DEFER-TT-08` 跟踪）。
- 不实现全局 `--git-dir`/`--work-tree`（`#801` 登记为 `DEFER-DESC-02`，已由 `plan-20261117-global-init-infra.md` `GII-02` 承接）。
- 不修改 `libra update-ref` 以支持 `refs/tags/*`（`src/command/update_ref.rs:6-7, 147`）；misnamed tag 测试夹具通过 `libra tag Q <tag-oid>`（`src/command/tag.rs:817-869` 目标类型检测保留未剥皮的 tag 对象）+ `libra tag -d A` 在 SQLite `reference` 表构造。
- 不改变 `--contains --all` 展示分支前缀 `heads/` 的现有格式（`DEFER-DESC-03`），也不改变既有的 `--contains --exact-match` 差异（`DEFER-DESC-04`）。
- 不新增 SQLite schema migration、不修改 `src/cli.rs` 子命令注册。

### 成功定义

- 上述 8 个 Open Issues 全部映射到具体任务卡并逐卡通过本地验收（`--test command_test` focused 用例、`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、nextest 发布门）、外部上游 `t6120-describe.sh`/TC 门（`DEP-DESC-TC`）、ER-05 review `PASS` 与 C/D 发布门。
- `--json` 与人类可读输出在所有新增/修复选项下均保持字段一致与可解析性。
- 中英文命令文档、`COMPATIBILITY.md`（原地更新第 219 行）与 `/Volumes/Sea/libra-backend` `cf` 分支网站文档随对应任务卡同卡同步。
- 全部任务卡达到 `done / complete` 且全量收口门通过后，本计划方可标记完成。

## 使用规则

- 日期计划正文、`plan-status.md` 状态推进与「修订历史」每次写入均记录 `YYYY-MM-DD HH:MM:SS UTC` 时间戳。
- 任何执行本计划的 Agent 在推进或完成一张任务卡时，必须在同一变更中同步更新 [plan-status.md](plan-status.md)（Phase 0 开工前须先在 `plan-status.md` 登记 `plan-20261117-describe.md` 状态行）。
- 禁止在 `docs/development/plan/` 或仓库其它位置新建 `evidence/`、`baseline/`、`scratch/` 等证据目录或文件；所有验收摘要、测试计数与门结果均写入本计划正文。
- 每个任务卡开工前必须执行 ER-01（`libra status --short --branch`）与 ER-02（刷新源码行号与当前行为复现）。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。以下基线核对自 `62abb622f0eb2626934633ada78061963ebf1f44`（`2026-11-17 02:50:04 UTC`）。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `DescribeArgs`、`describe_commit_for_atom`、`run_describe`、`run_describe_contains`、`apply_dirty_mark`、`describe_cli_error` | `src/command/describe.rs:60-115, 152-186, 188-381, 394-531, 533-561, 627-669` |
| 格式化逻辑 | `describe_output`、`abbreviate_hash`、`format_describe_result` | `src/command/describe_format.rs:3-50` |
| 类型与引用 | `DescribeOutput`、`DescribeError`；`TagObject::Tag(tg)` 的 `tg.tag_name` 承载内部标签名；`tag::list` 从 SQLite 读取 tag 引用；`tag.rs` 目标类型检测保留未剥皮 tag 对象 | `src/command/describe_types.rs:5-54`, `src/internal/tag.rs:370-410`, `src/command/tag.rs:817-869` |
| 修订解析 | `resolve_object_atom_typed` 仅尝试 ref 与纯十六进制前缀，不支持 `<tag>-<N>-g<sha>` | `src/utils/util.rs:2319-2335, 2404-2468` |
| CLI 注册 | `Commands::Describe(command::describe::DescribeArgs)` 路由到 `execute_safe` | `src/cli.rs:539, 3954` |
| 单元测试 | `describe_tests.rs` 覆盖错误文案与 `compile_globs`/`tag_passes_filters` | `src/command/describe_tests.rs:1-92` |
| 集成测试 | `describe_test`（850 行，含 `:687` 现有 `--contains --exact-match` 断言）、`describe_long_test`；均挂载在 `--test command_test` | `tests/command/mod.rs:1039-1040`, `tests/command/describe_test.rs:90-135, 687`, `tests/command/describe_long_test.rs:4-75` |
| 命令文档 | EN/zh-CN 命令页与相邻仓库 `/Volumes/Sea/libra-backend` 网站 EN 文档存在 | `docs/commands/describe.md:1-100`, `docs/commands/zh-CN/describe.md:1-100`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md` |
| 兼容矩阵 | `describe` 标为 `partial`（`tests/compat-ledger` 固定行号，必须原地更新） | `COMPATIBILITY.md:219` |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-DESC-01 | `run_describe_contains` 对附注 tag 精确匹配（`generation == 0`）未追加 `^0`，且在 `--contains` 中错误调用 `apply_dirty_mark` | 与 `git describe --contains` 不一致（#795） | `src/command/describe.rs:417, 427, 465-469, 496`, `tests/command/describe_test.rs:687` | `DESC-01` |
| GAP-DESC-02 | `DescribeArgs` 缺少 `--no-match`，无法重置先前的 `--match` 过滤模式 | 无法覆盖/清除 `--match`（#796） | `src/command/describe.rs:102-104` | `DESC-02` |
| GAP-DESC-03 | `--all` 加载 `heads/*` 与 `remotes/*` 时未应用 `--match`/`--exclude` | `--contains --all --match` 忽略 `--match` 并误匹配未过滤分支（#797） | `src/command/describe.rs:265-289` | `DESC-03` |
| GAP-DESC-04 | `resolve_object_atom_typed` 不支持 `<tag>-<N>-g<sha>` 语法 | `#798`/`#799` 的 `rev-parse` 往返验收无法解析 describe 输出 | `src/utils/util.rs:2319-2335, 2404-2468` | `DESC-09` |
| GAP-DESC-05 | `tag_map` 未使用 `TagObject::Tag(tg).tag_name`，且无 `misnamed` 标志在 `--abbrev=0` / `--no-long` 时强制输出 `-N-g<sha>` | misnamed annotated tag（`refs/tags/Q` → 内部名 `A`）在 `--abbrev=0`（#799）与 `--no-long`（#798）下输出错误 | `src/command/describe.rs:225-258`, `src/command/describe_format.rs:11-50` | `DESC-05` |
| GAP-DESC-06 | `DescribeArgs` 缺少 `--no-long`，无法取消先前的 `--long` 标志 | `--long --no-long` 被 clap 拒绝（#798） | `src/command/describe.rs:90-92, 195-198` | `DESC-04` |
| GAP-DESC-07 | `DescribeArgs` 缺少 `--no-abbrev`（Git 中等价于 `--abbrev=0`） | `--no-abbrev` 被 clap 拒绝（#800） | `src/command/describe.rs:74-80, 194-198, 362-378` | `DESC-06` |
| GAP-DESC-08 | `run_describe` 未在非 `--contains` 路径拒绝同时传入 `--dirty` 与 `<commit>` 参数 | `libra describe --dirty <commit>` 静默执行工作区脏检查（#802） | `src/command/describe.rs:62, 94-96, 188-190` | `DESC-07` |
| GAP-DESC-09 | `DescribeArgs` 缺少 `--broken[=<mark>]` 标志 | `libra describe --broken` 被 clap 拒绝（#803） | `src/command/describe.rs:94-96, 533-561` | `DESC-08` |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-long.md` | Git 命令兼容性长期能力 | 完成后按需更新日期计划索引，不复制长期路线图 |
| `plan-20261117-branch.md` | 写集包含 `src/utils/util.rs:2635-2675`（`BR-06`/`07`/`25..27`/`29..32`）、`tests/command/rev_parse_test.rs`、`rev-parse` 文档与 `COMPATIBILITY.md` | 通过 `DEP-DESC-UTIL` 与 `DESC-09`/`DESC-09b`/`DESC-09c`/`DESC-09a`/`DESC-10`/`DESC-10a..10k` 串行协调 `src/utils/util.rs`、`rev_parse_test.rs` 与 `rev-parse` 文档写集，避免 I–I 并发冲突 |
| `plan-20261117-clone.md`、`plan-20261117-merge-pull.md`、`plan-20261117-global-init-infra.md` | 写集包含 `src/utils/util.rs`（`CL-23`、 `MP-30`、`GII-03`/`GII-12`），其中 `GII-12`/`MP-30` 亦写 `rev_parse_test.rs` 与 `rev-parse` 文档（EN/zh-CN/网站） | 全部纳入 `DEP-DESC-UTIL`，在 `DESC-09`/`DESC-09b`/`DESC-09c`/`DESC-09a`/`DESC-10`/`DESC-10a..10k` 开工前核验上述计划写锁并刷新行号 |
| 同日其它计划（`checkout-switch-restore`、`commit-signing`、`diff`、`fetch-push`、`format-patch`、`grep`、`log-show`、`sequencer-am`、`worktree-clean-reset-apply`） | 触及 `COMPATIBILITY.md` 与版本发布 | `COMPATIBILITY.md` 仅原地修改 `:217` / `:219`，并由 `DEP-DESC-COMPAT` 在实现阶段串行；发布窗口遵循 ER-12 |
| `plan-20261117-plumbing-misc.md` | `PM-05`/`PM-06` 写 `tests/command/rev_parse_test.rs`、`rev-parse` 文档（EN/zh-CN/开发/网站），`PM-06` 还写 `COMPATIBILITY.md` | 通过 `DEP-DESC-UTIL` 与 `DESC-09`/`DESC-09b`/`DESC-09c`/`DESC-09a`/`DESC-10`/`DESC-10a..10k` 串行；`COMPATIBILITY.md` 另走 `DEP-DESC-COMPAT` |
| `docs/commands/describe.md` / `zh-CN/describe.md` / `COMPATIBILITY.md` | 命令用户文档与兼容矩阵 | 每张涉及公开 CLI 行为的卡按 ER-06a 同卡同步；`COMPATIBILITY.md:219` 仅原地更新 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 8 个 Open Issues 均属 `describe` Git 兼容缺口，补入 `DESC-09` 实现 `rev-parse` `<tag>-<N>-g<sha>` 往返以完整闭环 `#798`/`#799` | 按 `DESC-01 -> DESC-02 -> DESC-03 -> DESC-09 -> DESC-09b -> DESC-09c -> DESC-09a -> DESC-10 -> DESC-10a -> DESC-10b -> DESC-10c -> DESC-10d -> DESC-10e -> DESC-10f -> DESC-10g -> DESC-10h -> DESC-10i -> DESC-10j -> DESC-10k -> DESC-05 -> DESC-04 -> DESC-06 -> DESC-07 -> DESC-08` 排定 |
| 可行性 | `DESC-01..08` 集中在 `src/command/describe*.rs`；`DESC-09` 独立改动 `src/utils/util.rs` 并触发 `T-1` 全量门 | 均可在 M 规模内独立交付 |
| 任务卡粒度 | 每卡 1 个行为轴、AC 逐独立谓词精确计数且均 ≤ 8、VER ≤ 8、landing=1、scope=M | 满足 `G-01..11`，无需 waiver |
| 依赖与顺序 | `DESC-01..08` 共享 `src/command/describe.rs`；`DESC-09` 与 `plan-20261117-branch.md` 共享 `src/utils/util.rs` | 全串行并登记 `DEP-DESC-UTIL` 与 `DEP-DESC-DOCS` |
| 完整性 | 包含单元测试、nextest 集成门（含 `t6120` 场景与 `rev-parse` 往返）、EN/zh-CN/网站文档、`COMPATIBILITY.md:219`、可执行 `immutable-release` 降级指引与完整 C/D 门 | 全覆盖 ER-04/06a/08/13/14 |
| 安全性 | 不引入外部网络、不接触全局配置库、遵循 `LBR-CLI-002` fail-closed | 继承 GC-07/GC-11 |
| 功能正确性 | 已对照 `builtin/describe.c` 与 `t6120-describe.sh` 校准 `--contains` 忽略 `--dirty`/`--broken`、`--contains ^0` 仅限附注 tag、misnamed tag、`--no-abbrev == --abbrev=0`、`--broken` 隐含 `-dirty` | 逐卡附正向与负向用例 |
| 接口兼容 | 保持 `DescribeOutput` JSON 向后兼容（不删除或重命名已有字段） | 在 ADR-DESC-02 固定 |
| 数据流与控制流 | 纯只读仓库/工作区查询，无持久化状态写入 | 无并发写冲突 |
| 性能与容量 | 复用现有 BFS/Dijkstra 与 `status` 检查，不引入额外全仓扫描 | 继承 GC-10 |
| 可靠性与容错 | `apply_dirty_mark` 在 `--broken` 模式下显式区分 stat-only/干净、脏工作区（`-dirty`）与损坏工作区（`-broken`） | 在 `DESC-08` 处理 |
| 可维护性 | 参数 last-wins 全部通过 clap `overrides_with` 在 `DescribeArgs` 内聚完成 | 在 ADR-DESC-01 固定 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 02:52:00 UTC | 初稿建立 | 新建 `plan-20261117-describe.md`，覆盖 Issues #795-#800, #802-#803 | N/A | `DESC-01..08` |
| 2026-11-17 02:59:00 UTC | Claude & Codex R1 FAIL | 新增 `DESC-09`（misnamed tag）；修正 `DESC-01`/`DESC-06`/`--test command_test`/`Rollback mode=immutable-release`；登记 `DEFER-DESC-02` | N/A → 新增 `DESC-09` | 实施顺序、粒度表、任务卡、Review log |
| 2026-11-17 03:08:00 UTC | Claude & Codex R2 FAIL | 将原 misnamed tag 卡合并入 `DESC-05`；改用 SQLite tag 夹具；对齐 `--contains` 忽略 `--dirty`/`--broken` 与 `--broken` 隐含 `-dirty`；修正 `test_describe_*` 与 `COMPATIBILITY.md:219` 原地更新 | `DESC-09` 合并入 `DESC-05` | 实施顺序、粒度表、任务卡、Review log |
| 2026-11-17 03:15:00 UTC | Claude & Codex R3 FAIL | 新增 `DESC-09`（`util.rs` 解析 `<tag>-<N>-g<sha>`，`T-1`）及 `DEP-DESC-UTIL`；为 `DESC-02`/`04`/`06` 补齐 `--lib` nextest 门；重计所有卡 G-03 独立谓词数至 ≤ 8 且修正 `prod-files`；补全可执行 `immutable-release` 降级指引与 D 组具体 job/ref/取证命令；修正 `tag.rs` 锚点与同日计划关系；固定 `#795`/`#800` 上游用例形态与 `DEFER-DESC-04` | 新增 `DESC-09`（`rev-parse`） | 实施顺序、依赖表、全局默认、粒度表、任务卡、测试矩阵、追溯表、Review log |
| 2026-11-17 03:30:00 UTC | Codex R4 FAIL（Claude R4 ECONNRESET） | 修正 `install.sh` 降级参数为 `sh -s -- -v "v${PREV_VER}"` 及 `gh release download` 安装步骤；修正 D 组 CDN 具体 URL（`/libra/releases/v${VERSION}/libra-darwin-arm64`）与 `gh run view --json jobs` 逐 job 结论核验；将 `../libra-backend/.../rev-parse.en.md` 与 `COMPATIBILITY.md` 纳入 `DESC-09` 写集与 `DEP-DESC-DOCS`；拆分 `DESC-09` AC 失败路径至 6/8 | N/A | 全局默认、粒度表、`DESC-09`、Review log |
| 2026-11-17 03:38:00 UTC | Codex R5 FAIL（Claude R5 ECONNRESET） | 修正粒度审计表 `DESC-04` `VER=4/8` 与卡片一致；移除 `gh release download` 备选降级（`release.yml` 上传至 R2 CDN 而非 GitHub Release assets），保留并验证 `install.sh -v "v${PREV_VER}"` | N/A | 全局默认、粒度表、Review log |
| 2026-11-17 03:43:00 UTC | Codex R6 FAIL（Claude R6 ECONNRESET） | 区分未提交改动恢复（`libra restore --staged --worktree -- <card files>` 保留无关改动）与已提交未发布恢复（`libra revert <commit>`）；将后端文档交付改为直接 `cf` 分支，遇脏或超前提交即停止报告 | N/A | 依赖表、全局默认、故障恢复矩阵、风险登记、Review log |
| 2026-11-17 03:54:00 UTC | Codex R7 FAIL（Claude R7 ECONNRESET） | 在未提交恢复前增加 `libra status`/`libra diff` 差异归属核对门，禁止对混有同文件无关改动的文件执行整文件 `libra restore` | N/A | 全局默认、故障恢复矩阵、Review log |
| 2026-11-17 04:12:00 UTC | Claude R8 FAIL（Codex R8 PASS） | 修正 `DESC-09` 单测模块为 `utils::util::test` 并补齐 `rev_parse_test` 集成用例与 `COMPATIBILITY.md:217`；将 misnamed tag 处理限定于非 `--contains` 路径且仅对最终选定 tag 打印警告；按 G-03 严格消除合并谓词并重计全部卡片 `AC` 与审计表 | N/A | 粒度表、`DESC-01..10`、测试矩阵、追溯表、Review log |
| 2026-11-17 04:16:00 UTC | Codex R9 FAIL（Claude R9 PASS） | 新增 `DEP-DESC-TC` 外部上游 `t6120-describe.sh`（`@6de20f6092`）与 TC 验收门（不可用时 fail-closed 保留 Issue 未闭环，不宣称上游通过） | N/A | 成功定义、依赖登记表、测试矩阵、Review log |
| 2026-11-17 04:20:00 UTC | Claude & Codex R10 FAIL | 将 `DEP-DESC-TC` 接入 `DESC-01..08` 依赖、Verification 命令（`../libra-testcases/cases/tc1031..1054`）、审计表（`VER +1`）、M1/M2 与完成判据；固定完整上游 SHA `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd` 与前置子用例干扰处理规则 | N/A | 依赖登记表、粒度审计表、`DESC-01..08`、测试矩阵、里程碑、完成判据、Review log |
| 2026-11-17 04:26:00 UTC | Codex R11 FAIL（Claude R11 ECONNRESET） | 将 `DESC-05` AC 拆分为 7 项严格单一谓词（消除 `data.tag` 与 `data.abbreviated_commit` 合并），同步审计表与摘要为 `7/8` | N/A | 粒度审计表、`DESC-05`、Review log |
| 2026-11-17 04:33:00 UTC | Codex R12 FAIL（Claude R12 超时取消） | 将 `DESC-05` `--contains` 引用名输出与不打印 misnamed 警告拆分为 2 项独立谓词（`AC=8/8`），同步审计表与摘要 | N/A | 粒度审计表、`DESC-05`、Review log |
| 2026-11-17 04:38:00 UTC | Claude & Codex R13 FAIL | 拆分 `DESC-01`(7/8)、`DESC-03`(6/8)、`DESC-06`(8/8)、`DESC-07`(7/8)、`DESC-08`(8/8)、`DESC-09`(7/8) 全部双谓词 AC；明确 `DESC-05` AC3 目标 `c` 并收窄 `--all` 描述 | N/A | 粒度审计表、`DESC-01/03/05/06/07/08/09`、Review log |
| 2026-11-17 04:43:00 UTC | Codex R14 FAIL（Claude R14 PASS） | 将 `DESC-03` AC1、`DESC-07` AC1-2、`DESC-08` AC6 改为单一退出码或单一错误类型谓词（保持 `6/8`、`7/8`、`8/8`） | N/A | `DESC-03/07/08`、Review log |
| 2026-11-17 04:48:00 UTC | Codex R15 FAIL（Claude R15 取消） | 收紧 `DEP-DESC-TC` 与完成判据为严格 `tc*.sh` 退出码 0 门禁（映射子用例未通过或受前序级联失败均保持 `blocked`） | N/A | 依赖登记表、完成判据、Review log |
| 2026-11-17 04:52:00 UTC | Claude & Codex R16 FAIL | 注册 `DEP-DESC-541`（`#541` `update-ref refs/tags/*`）于 `DESC-04`/`05`；要求 `TC-<id> PASS` 精确输出行与退出码 0，并记录 `TC_RUN_DIR` 上游 `exit` 文件 | N/A | 成功定义、依赖表、审计表、`DESC-04/05`、M1、风险、完成判据、Review log |
| 2026-11-17 04:59:00 UTC | Codex R17 FAIL（Claude R17 ECONNRESET） | 在 `DESC-09` 补充全长（40/64 位）与缩写 `<sha>` 后缀的对象存在性及提交类型校验（`util.rs:2319` 原有全长哈希不校验存在性，须仅对 `-<N>-g<hex>` 回退路径强制校验，保持 `AC=8/8`） | N/A | 审计表、`DESC-09`、Review log |
| 2026-11-17 05:02:00 UTC | Codex R18 FAIL（Claude R18 取消） | 消除 `DEP-DESC-DOCS` 硬编码 `/Volumes/Data/gitmono/libra-backend`（改用 `../libra-backend` 并要求开工前核验实际站点 remote 与 `cf` 分支）；`DESC-01` 补齐 `data.exact_match == true`（`8/8`）；`DESC-09` 补齐 `tags/`/`heads/`/`remotes/` 命名空间回退（`util.rs:2438`） | N/A | 依赖表、审计表、`DESC-01/09`、Review log |
| 2026-11-17 05:06:00 UTC | Codex R19 FAIL（Claude R19 取消） | 按 G-01..G-03 将 `DESC-09` 拆分为 `DESC-09`（`<tag>-<N>-g<sha>` 核心解析与 OID 校验，`7/8`）与 `DESC-10`（`tags/`/`heads/`/`remotes/` 命名空间前缀回退，`5/8`），同步依赖链、审计表与追溯表 | `DESC-10` 拆自 `DESC-09` | 实施顺序、Phase 1、审计表、`DESC-09/10/05`、追溯表、Review log |
| 2026-11-17 05:10:00 UTC | Codex R20 FAIL（Claude R20 取消） | 将 `DEP-DESC-DOCS` 校验命令改为 `../libra-backend/package.json` 实际脚本 `pnpm --filter @tinyship/tanstack-app typecheck && pnpm --filter @tinyship/tanstack-app build` 并核验 `origin` 为 `libra-tools/libra-backend`；统一故障恢复矩阵保留无关改动规则；补齐 `DESC-10` 至追溯表、Phase 1 与 M1 | N/A | 依赖表、Phase 1、M1、故障恢复矩阵、追溯表、Review log |
| 2026-11-17 05:13:00 UTC | Codex R21 FAIL（Claude R21 取消） | 在 `M1` 补齐 `DESC-10`；在 `DESC-09` 补齐 G-09 拆出 `DESC-10` 声明；统一故障恢复矩阵与风险登记保留无关未提交改动规则 | N/A | `DESC-09`、M1、故障恢复矩阵、风险登记、Review log |
| 2026-11-17 05:16:00 UTC | Codex R22 FAIL（Claude R22 取消） | 在 Phase 1 退出条件、适用范围与关联计划表补齐 `DESC-10` | N/A | 适用范围、关联表、Phase 1、Review log |
| 2026-11-17 05:19:00 UTC | Codex R23 FAIL（Claude R23 取消） | 将 `DEP-DESC-UTIL` 开工前核验与暂停策略扩展至 `DESC-09` 与 `DESC-10`；在测试矩阵单元行补齐 `resolve_namespaced_describe_output_suffix` | N/A | 依赖表、测试矩阵、Review log |
| 2026-11-17 05:28:00 UTC | Codex R25b FAIL | 在 `DEP-DESC-DOCS` 补充 `/Volumes/Sea/libra-backend` 缺失时向协调者确认门与 `sync-libra-command-docs.py --check` | N/A | 依赖表、Review log |
| 2026-11-17 05:33:00 UTC | Codex R26 FAIL（Claude R26 取消） | 在 `DEP-DESC-DOCS` 补齐最终网站文档变更的 Claude Code `PASS` 门禁（`cf` 提交、推送、部署及卡完成前置） | N/A | 依赖表、Review log |
| 2026-11-17 06:53:00 UTC | Claude & Codex R27 FAIL | ① `DEP-DESC-DOCS` 按命令分流 `--write --only` / `--check --only`（`describe` vs `rev-parse`），补齐 `.libra`/`.git` 单一 VCS 核验并修正 `AGENTS.md:135` 归属；② `DEP-DESC-TC` 强制唯一 `TC_RUN_ID` 并记录二进制 SHA-256 与 `HEAD` SHA；③ D 组核对实际 `jobs[].name`（`compat-*`/`security-codeql-*`）与 `release.yml` 4-leg matrix/event/ref；④ `DEP-DESC-UTIL` 与同日计划表修正并补全 `branch`/`clone`/`merge-pull`/`global-init-infra` 写集协调；⑤ `DEP-DESC-541` 允许 `DESC-05`/`04` 完成 C/D 组（`Acceptance=complete`）仅保留 `TC-1043`/`1045` 与关单为 `Lifecycle=blocked`，并设 14 天超时升级策略；⑥ `DESC-01` AC8 收窄至普通 `--contains`（`8/8`），`DESC-09` 拆分全长/缩写校验并移除冗余 `~1` AC（`8/8`）；⑦ 同步 `DEFER-DESC-02`（`GII-02`）、`Phase 0` `plan-status.md` 登记门与 `DESC-10` 追溯表测试函数名 | N/A | 依赖表、同日计划表、全局默认、Phase 0/1、`DESC-01/04/05/08/09`、测试矩阵、追溯表、里程碑、风险登记、`DEFER-DESC-02`、完成判据、Review log |
| 2026-11-17 07:42:48 UTC | Codex R28 FAIL（Claude R28 `ECONNRESET`，随 R29 重跑） | `DEP-DESC-541` 不再允许 `DESC-04`/`DESC-05` 在 TC 未通过时进入 C/D 或 `Acceptance=complete`；Phase 1、M1、风险表与完成判据改为 `#541` 合入且 `TC-<id> PASS` 之后才发布 | N/A | 依赖表、Phase 1、里程碑、风险表、完成判据、Review log |
| 2026-11-17 08:05:51 UTC | Codex R29 FAIL | Phase 1 退出条件补入 `DESC-06` `done/complete`，并拆开 `TC-1045`（`DESC-05`）与 `TC-1043`（`DESC-04`）的前置 | N/A | Phase 1、M1、`DEP-DESC-541`、Review log |
| 2026-11-17 08:28:29 UTC | Codex R31 FAIL + Claude R32 FAIL | 新增 `DEP-DESC-COMPAT` 并接入 `DESC-01`..`DESC-10` 的依赖、Granularity 与审计表；`DESC-01` 审计轴去掉尚未实现的 `--broken` | N/A | 依赖表、全部任务卡、审计表、Review log |
| 2026-11-17 08:45:00 UTC | Claude R34 FAIL（Codex R34 PASS，因本修订失效） | `DEP-DESC-UTIL` 纳入 `plumbing-misc` `PM-05`/`PM-06`；`DESC-01..08` 写集补 `docs/development/commands/describe.md`，`DESC-09`/`10` 补 `docs/development/commands/rev-parse.md`；`DESC-01..08` 的 `Full-suite trigger` 改为版本面 `T-1` | N/A | 关系表、依赖表、写集、Review log |
| 2026-11-17 12:01:37 UTC | Codex R35 部分检查与用户单审指令 | R35 仅完成局部检查，无新增 P1，但不作为完整结论；计划提交门改为 Codex 单审 | N/A | 头部、`DEP-DESC-DOCS`、完成判据、Review log |
| 2026-11-17 12:27:28 UTC | Codex R36 FAIL | Phase 0 改为 Codex 单审；区分计划单审与网站交付门，恢复 `cf` 提交/推送/部署前独立 Claude Code PASS；同步文头最近修订时间 | N/A | 头部、Phase 0、`DEP-DESC-DOCS`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R37 部分检查 | R37 未完整复审且无新增 P1；将现行合同与写集网站路径统一为 `/Volumes/Sea/libra-backend`，取消其它 checkout 回退，保留历史评审记录原文 | N/A | `DEP-DESC-DOCS`、全部现行网站写集、Review log |
| 2026-11-17 12:27:28 UTC | Codex R38-A FAIL/P1 | 修正 DESC-09 错误的提交类型限制，要求 blob/tree/commit/tag 全长及缩写对象对照 Git，并保留历史 R17 记录 | N/A | `DESC-09`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R39-A FAIL/P1 | DESC-10 扩展完整 refs 前缀与非提交对象回退、字面引用优先及 Git 对照测试 | N/A | `DESC-09`、`DESC-10`、粒度审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R40-A FAIL/P1 | 修正 DESC-09 字面引用优先与无效语法冲突；DESC-09/10 独立判据超限待进一步拆分 | N/A | `DESC-09`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R40-A 粒度修订 | 拆出 `DESC-09a` 非提交对象与 `DESC-10a` 完整 refs 前缀，独立发布并配合同卡回归及文档；同步顺序、审计和完成门 | `DESC-09` → `DESC-09,09a`；`DESC-10` → `DESC-10,10a` | 任务卡、DAG、审计表、发布窗口、完成门 |
| 2026-11-17 12:27:28 UTC | Codex R41-A FAIL/P1 | `DESC-09a`/`DESC-10a` 文档字段逐文件登记；独立判据计数及发布切片仍需重拆，不以此轮判 PASS | N/A | 两张新卡、Review log |
| 2026-11-17 12:27:28 UTC | Codex R41-A 粒度修订 | DESC-09a 承接无效后缀字面引用优先，六种命名空间各拆为独立行为卡并逐项列出八种对象/长度判据及字面引用优先，串行发布 | DESC-10 → DESC-10..10e | 任务卡、DAG、审计表、发布窗口、完成门 |
| 2026-11-17 12:27:28 UTC | Codex R42-A FAIL/P1 | 所有合法字面引用优先归回 DESC-09；缺失和歧义的具体错误处理拆为 DESC-09b，保持中间版本 fail-closed | DESC-09 → DESC-09,09b | 任务卡、DAG、审计表、发布窗口、追溯 |
| 2026-11-17 16:05:00 UTC | Codex R43-A FAIL/P1 | 新增 DESC-09c 明确附注 tag 对象全长与缩写 OID，并扩展命名空间覆盖；DESC-10 组合表达式测试固定父提交和 Git 对照 | DESC-09 → DESC-09c | 任务卡、DAG、审计表、发布链、追溯 |
| 2026-11-17 16:20:00 UTC | Codex R44-A 无有效输出 | 子代理完成但输出产物为空，未形成 PASS/FAIL；按未完成处理，不据此推进 | N/A | 状态、Review log |
| 2026-11-17 17:40:00 UTC | Codex R45-A/R46-A FAIL/P1 | DESC-09a 补 tag 对象身份回归；为六种命名空间新增 tag 对象完整与缩写 OID 的独立功能卡，明确前序卡 fail-closed 中间边界并串行发布 | DESC-10 → DESC-10f..10k | 任务卡、DAG、审计表、发布窗口、追溯、完成门 |

## 已决议设计决策

### ADR-DESC-01: 通过 clap `overrides_with` 实现 `--no-long`、`--no-match`、`--no-abbrev` 的 last-wins 语义

- **Status:** Accepted
- **Context:** Git 的 `parse-options` 允许 `--long --no-long`、`--match a --no-match --match b`、`--abbrev=4 --no-abbrev` 在同一命令行出现，后出现的标志覆盖先前设置。本计划不修改 `src/cli.rs` 全局解析逻辑。
- **Decision:** 在 `DescribeArgs` 中全部采用 clap 原生 `overrides_with` 与对称布尔/值标志实现 last-wins，并同步更新 `describe_commit_for_atom`（`src/command/describe.rs:159-173`）的 `DescribeArgs` 结构体字面量默认值。
- **Alternatives considered:** 修改 `src/cli.rs` 注入位置索引，会触发 ER-13 `T-1` 跨切面修改且超出 `describe` 单一模块范围。
- **Consequences:** 无须改动 `src/cli.rs`；全部 CLI 选项覆盖行为可在 `DescribeArgs::try_parse_from` 单元测试（`src/command/describe_tests.rs`）中直接验证。
- **Revisit when:** Clap 升级或仓库引入统一的 Git `parse-options` 兼容层时。

### ADR-DESC-02: 保持 `DescribeOutput` JSON 契约向后兼容

- **Status:** Accepted
- **Context:** `libra describe --json` 已有稳定的 `DescribeOutput` 结构（`src/command/describe_types.rs:5-18`）。
- **Decision:** 本计划修复中不重命名或删除任何现有 `DescribeOutput` 字段；`--contains` 的 `^0` 仅反映在 `result`（`tag` 仍为纯 tag 名，`exact_match` 仍为 `true`）；misnamed tag 下 `data.tag` 保留内部 tag 名（如 `"A"`），`data.abbreviated_commit` 为实际使用的缩写/完整哈希（在 misnamed tag + `--abbrev=0` 强制输出后缀时为完整 commit SHA）；`--broken` 通过 `dirty` / `dirty_mark` 表达，不破坏现有 JSON 解析器。
- **Alternatives considered:** 修改 `DescribeOutput` 现有字段类型或删除字段，会破坏既有自动化调用方。
- **Consequences:** 所有现有 `--json` 测试持续兼容。
- **Revisit when:** 未来发布主版本重构 JSON schema 时。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 全部 `GC-01..13`：
- **GC-01** 现状核实前置；**GC-02** 单一事实源；**GC-03** Git 互操作边界；**GC-04** 输出与错误契约（`LBR-CLI-002` 等）；**GC-05** 文档同步（`COMPATIBILITY.md:219` 仅原地修改，禁止增删行破坏 `tests/compat-ledger` 固定行号）；**GC-06** 测试索引；**GC-07** 安全默认值；**GC-08** 原子性与恢复；**GC-09** 并发与资源生命周期；**GC-10** 性能预算；**GC-11** 生产 panic 禁止；**GC-12** 精确暂存；**GC-13** 数据库迁移作用域与全局配置隔离（本计划不触及 SQLite schema）。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 全部 `ER-01..14`：
- **ER-01** 开工前 `libra status --short --branch` 检查；**ER-02** 刷新锚点与复现；**ER-03** 粒度门禁；**ER-04** A/B/C/D 四组验收门；**ER-05** review 闭环；**ER-06 / ER-06a** 同卡 EN/zh-CN/网站/COMPATIBILITY 同步（写入 `/Volumes/Sea/libra-backend` 前核验 VCS 与 `cf` 分支）；**ER-07** Libra-native 签名+DCO 提交；**ER-08** 每卡 `patch + 1` + 版本面同步 + 工具链刷新 `Cargo.lock` + `gh release create`；**ER-09 / ER-10** 失败策略；**ER-11** 证据卫生；**ER-12** 串行发布；**ER-13** 执行阶段 focused 测试 + 全量收口门；**ER-14** nextest 作为发布集成测试唯一执行器（每张发布卡在 C 组第 ④ 步必须以 `source .env.test && cargo nextest run ...` 覆盖本卡 A 组全部 focused 门）。

## 实施顺序

因 `DESC-01..08` 共享 `src/command/describe.rs` 与命令文档，`DESC-09` 为 `DESC-05`/`DESC-04` 往返解析前置，全链按 G-10 串行执行：

- `DESC-01 -> DESC-02`
- `DESC-02 -> DESC-03`
- `DESC-03 -> DESC-09`
- `DESC-09 -> DESC-09b`
- `DESC-09b -> DESC-09c`
- `DESC-09c -> DESC-09a`
- `DESC-09a -> DESC-10`
- `DESC-10 -> DESC-10a`
- `DESC-10a -> DESC-10b`
- `DESC-10b -> DESC-10c`
- `DESC-10c -> DESC-10d`
- `DESC-10d -> DESC-10e`
- `DESC-10e -> DESC-10f`
- `DESC-10f -> DESC-10g`
- `DESC-10g -> DESC-10h`
- `DESC-10h -> DESC-10i`
- `DESC-10i -> DESC-10j`
- `DESC-10j -> DESC-10k`
- `DESC-10k -> DESC-05`
- `DESC-05 -> DESC-04`
- `DESC-04 -> DESC-06`
- `DESC-06 -> DESC-07`
- `DESC-07 -> DESC-08`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-DESC-DOCS | incoming | 相邻仓库文档分支 | `/Volumes/Sea/libra-backend`（唯一网站工作区，`cf` 生产分支；路径不可用、存在双 VCS 或不在 `cf` 即 STOP，禁止改用其它 checkout） | 当前执行者 | 写入前核验 `/Volumes/Sea/libra-backend` 仅存在 `.libra` 且无 `.git`（ER-06a 单一 VCS 检查），再执行 `cd /Volumes/Sea/libra-backend && libra status --short --branch && libra remote -v` 并把结果写入本计划（确认 `origin` 指向 `libra-tools/libra-backend`、位于 `cf` 分支且目标 `.en.md` 可更新；保留已有无关未提交改动，禁止擅自 `restore`/`checkout` 丢弃）；改动后按本卡所属命令执行 `python3 /Volumes/Sea/libra-backend/scripts/sync-libra-command-docs.py --libra-root . --backend-root /Volumes/Sea/libra-backend --write --only <cmd>` 与 `--check --only <cmd>`（`DESC-01..08` 用 `describe`；`DESC-09`/`DESC-09b`/`DESC-09c`/`DESC-09a`/`DESC-10`/`DESC-10a..10k` 用 `rev-parse`，不得跨命令连带无关未授权页面），再在 `/Volumes/Sea/libra-backend` 执行 `pnpm --filter @tinyship/tanstack-app typecheck && pnpm --filter @tinyship/tanstack-app build`，取得覆盖最终网站文档变更的 Claude Code `PASS`（该网站交付门独立于计划 Codex 单审）后，方可在 `cf` 分支签名提交（仅暂存本卡 `.en.md`）、推送与确认 Cloudflare 部署并标卡完成 | `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/{describe,rev-parse}.en.md`（2026-11-17 核对存在） | 若 `/Volumes/Sea/libra-backend` 缺失、存在双 VCS、非 `cf` 分支或含无关冲突导致无法安全更新，立即停止并向协调者报告，禁止擅自另建分支、worktree、独立克隆或改用其它 checkout；未完成前不得标 `complete` |
| DEP-DESC-COMPAT | incoming | 共享文件写集 | `COMPATIBILITY.md`（本计划只改 `:217` 与 `:219`） | 协调者 | 每张 `DESC-*` 开工前确认没有其它在跑卡正在修改 `COMPATIBILITY.md`；取得该文件的串行编辑窗口后才修改、验收并提交，提交后释放 | 本计划依赖表 | 未释放则等待，禁止与其它卡并发改同一文件（G-10） |
| DEP-DESC-UTIL | incoming | 跨计划写集协调 | `plan-20261117-branch.md`（`BR-06/07/25..27/29..32` 写 `src/utils/util.rs:2635-2675`、`tests/command/rev_parse_test.rs`、`rev-parse` 文档）、`plan-20261117-clone.md`（`CL-23` 写 `util.rs:277,314-324,797`）、`plan-20261117-merge-pull.md`（`MP-30` 写 `util.rs:3120`、`rev-parse` 文档）、`plan-20261117-global-init-infra.md`（`GII-03`/`GII-12` 写 `util.rs:3128`、`rev_parse_test.rs`、`rev-parse` 文档）、`plan-20261117-plumbing-misc.md`（`PM-05`/`PM-06` 写 `rev_parse_test.rs` 与 `rev-parse` 文档；`BR-28` 写 `util.rs`，`CL-14` 写 `rev_parse_test.rs`） | 协调者 | `DESC-09`、`DESC-09b`、`DESC-09c`、`DESC-09a`、`DESC-10` 与 `DESC-10a..10k` 各自开工前均核验上述 5 份计划无并发修改 `src/utils/util.rs`、`tests/command/rev_parse_test.rs`、`docs/commands/{,zh-CN/}rev-parse.md`、`docs/development/commands/rev-parse.md` 或 `/Volumes/Sea/libra-backend/.../rev-parse.en.md` 的在跑卡，并刷新 `util.rs:2319-2468` 行号 | `docs/development/plan/plan-20261117-{branch,clone,merge-pull,global-init-infra,plumbing-misc}.md`、`src/utils/util.rs:2404` | 若上述任一计划正在修改共享文件，暂停 `DESC-09`、`DESC-09b`、`DESC-09c`、`DESC-09a`、`DESC-10` 与 `DESC-10a..10k` 直至写集释放并重新 rebase |
| DEP-DESC-541 | incoming | 跨计划/上游能力前置 | `issues/541.md`（`#541`：`update-ref` 支持 `refs/tags/*`） | `update-ref` 计划负责人 | `libra update-ref refs/tags/Q <oid>` 与 `libra update-ref -d refs/tags/A` 可在 `t6120:123-132` 正常执行 | `docs/development/plan/issues/541.md`、`src/command/update_ref.rs:6-10,147` | 门按卡分开，避免 `TC-1043`（`describe --no-long`，由 `DESC-04` 实现）反向阻塞 `DESC-05`。`DESC-05`：`#541` 未合入或 `TC-1045` 未 `PASS` 前保持 `blocked`、`Acceptance` 为空，不得进入 C/D。`DESC-04`：在 `DESC-05` 已 `done/complete` 且 `#541` 已合入、`TC-1043` 已 `PASS` 之前同样保持 `blocked`。`DESC-06` 依赖 `DESC-04`。仓内 focused 测试可先写但不得发布。14 天未合入由协调者升级重排 `#541` |
| DEP-DESC-TC | incoming | 外部上游与 TC 验收夹具 | `../libra-testcases/cases/tc1031..tc1054`（`t/t6120-describe.sh` @ `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`） | 协调者 / 兼容性测试运行者 | 每次运行必须显式生成唯一 `TC_RUN_ID="desc-<card>-$(date -u +%Y%m%dT%H%M%SZ)-$$"`（禁止复用已含 `exit` 文件的 `TC_RUN_DIR`，防止 `tc-harness.sh:290` 命中缓存旧结果），执行本卡对应 `tc*.sh` 脚本（`tc-harness.sh:486,580` 按 `results.tsv` 汇总本卡映射的 `t6120` 子用例），取得脚本退出码 0 **且标准输出包含精确行 `TC-<id> PASS`**（排除 `SKIP`/`XFAIL` 伪零退出），并将 `TC_RUN_ID`、`shasum -a 256 "$PWD/target/debug/libra"`、`libra rev-parse HEAD`、`tc*.sh` 退出码与 `TC_RUN_DIR` 中上游测试进程 `exit` 文件值一并写入卡片记录（ER-11）；仅当 `tc*.sh` 退出 0 且输出 `TC-<id> PASS` 后方可将对应卡标 `done / complete` 并关闭 Issue | `../libra-testcases/cases/tc1031-describe-contains/tc1031-describe-contains.sh` 等 8 个脚本（2026-11-17 核对存在） | 若 `../libra-testcases` 目录/脚本缺失、上游检出非 `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd` 或对应 `tc*.sh` 返回非 0（`TC-<id> FAIL/XPASS`），相关卡置为 `Lifecycle=blocked` 且 `Acceptance` 为空（未过 TC 前不得进入 C/D，不得标 `complete`/`done`，也不得关闭对应 Issue） |

### 发布分组与并发窗口

无 `REL-*` 家族合并发布，每卡均为 `independent` 独立发布切片。

**并发声明:** 全串行（`DESC-01..08` 共享 `src/command/describe.rs`；`DESC-09` 需串行协调 `src/utils/util.rs`）。

**发布者:** 由协调者指定的单一发布者串行执行 C 组 bump/构建/提交/推送/`gh release create` 并跟踪 D 组结果（ER-12）。

**发布窗口顺序:** `DESC-01` → `DESC-02` → `DESC-03` → `DESC-09` → `DESC-09b` → `DESC-09c` → `DESC-09a` → `DESC-10` → `DESC-10a` → `DESC-10b` → `DESC-10c` → `DESC-10d` → `DESC-10e` → `DESC-10f` → `DESC-10g` → `DESC-10h` → `DESC-10i` → `DESC-10j` → `DESC-10k` → `DESC-05` → `DESC-04` → `DESC-06` → `DESC-07` → `DESC-08`。

### Phase 0: 基线核对与文档分支确认

**目标:** 确认工作区干净、上游基线与 `/Volumes/Sea/libra-backend` `cf` 分支状态。

**进入条件:**
- 计划经 Codex 单审返回 `VERDICT: PASS`。
- [plan-status.md](plan-status.md) 已登记 `plan-20261117-describe.md` 状态行。

**退出条件:**
- `ER-01` 与 `DEP-DESC-DOCS` 核验记录就绪。

### Phase 1: `describe` 过滤、`rev-parse` 往返与 Misnamed Tag / 缩写修复（`DESC-01..03`, `DESC-09`, `DESC-09a`, `DESC-10`, `DESC-10a..10k`, `DESC-05`, `DESC-04`, `DESC-06`）

**目标:** 依次修复 `--contains ^0`、`--no-match`、`--all` + `--match`、`rev-parse` `<tag>-<N>-g<sha>` 解析、misnamed annotated tag（`--abbrev=0`）、`--no-long` 与 `--no-abbrev`。

**进入条件:**
- Phase 0 退出条件满足。

**退出条件:**
- `DESC-01..03`、`DESC-09`、`DESC-09b`、`DESC-09c`、`DESC-09a`、`DESC-10`、`DESC-10a..10k` 全部 `done / complete`。`DESC-05` 在 `#541` 合入且 `TC-1045` `PASS` 后 `done / complete`。`DESC-04` 在 `DESC-05` `done / complete` 且 `#541` 合入且 `TC-1043` `PASS` 后 `done / complete`。`DESC-06` 随后 `done / complete`。未满足各自 TC 前对应卡 `Lifecycle=blocked`、`Acceptance` 为空。

### Phase 2: `--dirty` 互斥校验与 `--broken` 支持（`DESC-07..08`）

**目标:** 修复 `--dirty <commit>` 校验并实现 `--broken[=<mark>]`，随后执行计划级全量收口门。

**进入条件:**
- `DESC-06` 达到 `done / complete`。

**退出条件:**
- `DESC-07..08` 全部 `done / complete`，且全量收口门（fmt + clippy + `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`）全绿。

## 任务卡

### 字段全局默认与例外

- **Release boundary 默认:** `independent`
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`
- **Migration and rollback 默认:**
  - `无 schema 迁移`；兼容窗口：本计划不改动仓库存储格式与 SQLite schema，新旧二进制可零迁移直接互换降级。
  - **发布前登记：** 执行 bump 前先固定上一已知可用版本 `PREV_VER="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"` 并写入本卡记录。未提交前恢复须先核对 `libra status --short --branch` 与 `libra diff` 确认文件差异归属：仅当目标文件只含本卡改动时对已跟踪文件执行 `libra restore --staged --worktree -- <card files>` 并移除本卡新增未跟踪文件；若同一文件混有无关改动则停止并采用可验证的选择性 hunk 恢复或向协调者报告，禁止整文件覆盖丢弃同文件无关改动。若已本地提交但未推送发布，执行 `libra revert <commit>`。
  - **已发布降级指引：** 若已发布版本出现回归，用户先从本卡发布记录读取 `PREV_VER`（发布前须核验 `curl -fsSLI "https://download.libra.tools/libra/releases/v${PREV_VER}/libra-darwin-arm64"` 返回 HTTP 200），再执行 `curl -fsSL https://download.libra.tools/install.sh | sh -s -- -v "v${PREV_VER}"` 立即降级，同时按 ER-04 前滚发布新 patch 版本修复，不得撤回已发布 tag。
- **Security and privacy 默认:** `继承 GC-07、GC-11`
- **Performance budget 默认:** `继承 GC-10`
- **D 组远端后置门默认（逐项登记 workflow / job / 触发事件与 ref / 判据）:**
  - `.github/workflows/base.yml`（YAML job IDs：`format`、`clippy`、`redundancy`、`test`；`gh run view --json jobs` 返回的 `jobs[].name` 为 `compat-rustfmt`、`compat-clippy`、`compat-redundancy`、`compat-offline-core`；触发：`pull_request` / `push` `main`）→ `BASE_RUN_ID="$(gh run list --workflow=base.yml --commit "$MERGE_SHA" --json databaseId --jq '.[0].databaseId')"` 后执行 `gh run watch "$BASE_RUN_ID" --exit-status` 与 `gh run view "$BASE_RUN_ID" --json event,headBranch,headSha,jobs --jq '{event,headBranch,headSha,jobs:[.jobs[] | {name, conclusion}]}'`，确认 `headSha == "$MERGE_SHA"` 且上述 4 个 `jobs[].name` 全部为 `success`（任一非 `success` 则失败，按 ER-04/ER-10 前滚修复）。
  - `.github/workflows/codeql.yml`（YAML job ID：`analyze`；`jobs[].name` 为 `security-codeql-rust` 等 `security-codeql-${{ matrix.language }}`；触发：`pull_request` / `push` `main`）→ `CODEQL_RUN_ID="$(gh run list --workflow=codeql.yml --commit "$MERGE_SHA" --json databaseId --jq '.[0].databaseId')"` 后执行 `gh run watch "$CODEQL_RUN_ID" --exit-status` 与 `gh run view "$CODEQL_RUN_ID" --json event,headBranch,headSha,jobs --jq '{event,headBranch,headSha,jobs:[.jobs[] | {name, conclusion}]}'`，确认 `headSha == "$MERGE_SHA"` 且全部 `security-codeql-*` job 均为 `success`。
  - `.github/workflows/release.yml`（YAML job IDs：`build-and-upload`（4-leg matrix：`linux/amd64`、`linux/arm64`、`darwin/arm64`、`windows/amd64`）、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest`；触发：`push` `v*` tag，由 `gh release create "v${VERSION}" --target "$MERGE_SHA"` 触发）→ `REL_RUN_ID="$(gh run list --workflow=release.yml --branch "v${VERSION}" --json databaseId --jq '.[0].databaseId')"` 后执行 `gh run watch "$REL_RUN_ID" --exit-status` 与 `gh run view "$REL_RUN_ID" --json event,headBranch,headSha,jobs --jq '{event,headBranch,headSha,jobs:[.jobs[] | {name, conclusion}]}'`，确认 `event == "push"`、`headBranch == "v${VERSION}"`、`headSha == "$MERGE_SHA"` 且全部 8 个 `jobs[]`（含 4 个 `build-and-upload` 矩阵 leg 与其余 4 个独立 job）均为 `success`；随后执行 `curl -fsSLI "https://download.libra.tools/libra/releases/v${VERSION}/libra-darwin-arm64"`、`curl -fsSLI "https://download.libra.tools/libra/releases/v${VERSION}/libra-linux-amd64"` 与 `curl -fsSLI "https://download.libra.tools/libra/releases/v${VERSION}/libra-linux-arm64"`，均须返回 HTTP 200。

**默认覆盖：** 无。

**规则 waiver（`EX-*`）：** 无。

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| DESC-01 | implementation | `--contains` 附注 tag `^0` 与忽略 `--dirty` | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 8/8 | 3/8 | 1/1 | M | DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-01..10 | independent | N/A | N/A |
| DESC-02 | implementation | `--no-match` 重置 `--match` 过滤 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 5/8 | 3/8 | 1/1 | M | DESC-01, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-01..10 | independent | N/A | N/A |
| DESC-03 | implementation | `--all` 候选受 `--match`/`--exclude` 过滤 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 6/8 | 3/8 | 1/1 | M | DESC-02, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-01..10 | independent | N/A | N/A |
| DESC-09 | implementation | `rev-parse` 解析 `<tag>-<N>-g<sha>` 格式与 OID 校验 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 7/8 | 2/8 | 1/1 | M | DESC-03, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-01..10 | independent | N/A | N/A |
| DESC-09b | implementation | 后缀 OID 缺失及歧义错误 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 3/8 | 2/8 | 1/1 | M | DESC-09, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-09 | independent | DESC-09 | N/A |
| DESC-09c | implementation | 附注 tag 对象后缀解析 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 2/8 | 4/8 | 1/1 | M | DESC-09b, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-09b | independent | DESC-09 | N/A |
| DESC-09a | implementation | 无命名空间 blob/tree 后缀解析 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 4/8 | 4/8 | 1/1 | M | DESC-09c, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-09c | independent | DESC-09 | N/A |
| DESC-10 | implementation | `tags/` 命名空间后缀回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 8/8 | 2/8 | 1/1 | M | DESC-09a, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-09a | independent | DESC-09 | N/A |
| DESC-10a | implementation | `heads/` 命名空间后缀回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 7/8 | 2/8 | 1/1 | M | DESC-10, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10 | independent | DESC-09 | N/A |
| DESC-10b | implementation | `remotes/` 命名空间后缀回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 7/8 | 2/8 | 1/1 | M | DESC-10a, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10a | independent | DESC-09 | N/A |
| DESC-10c | implementation | `refs/tags/` 命名空间后缀回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 7/8 | 2/8 | 1/1 | M | DESC-10b, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10b | independent | DESC-09 | N/A |
| DESC-10d | implementation | `refs/heads/` 命名空间后缀回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 7/8 | 2/8 | 1/1 | M | DESC-10c, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10c | independent | DESC-09 | N/A |
| DESC-10e | implementation | `refs/remotes/` 命名空间后缀回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 7/8 | 2/8 | 1/1 | M | DESC-10d, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10d | independent | DESC-09 | N/A |
| DESC-10f | implementation | `tags/` 附注 tag 后缀回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 2/8 | 2/8 | 1/1 | M | DESC-10e, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10e | independent | DESC-09 | N/A |
| DESC-10g | implementation | `heads/` 附注 tag 后缀回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 2/8 | 2/8 | 1/1 | M | DESC-10f, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10f | independent | DESC-09 | N/A |
| DESC-10h | implementation | `remotes/` 附注 tag 后缀回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 2/8 | 2/8 | 1/1 | M | DESC-10g, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10g | independent | DESC-09 | N/A |
| DESC-10i | implementation | `refs/tags/` 附注 tag 后缀回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 2/8 | 2/8 | 1/1 | M | DESC-10h, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10h | independent | DESC-09 | N/A |
| DESC-10j | implementation | `refs/heads/` 附注 tag 后缀回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 2/8 | 2/8 | 1/1 | M | DESC-10i, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10i | independent | DESC-09 | N/A |
| DESC-10k | implementation | `refs/remotes/` 附注 tag 后缀回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 2/8 | 2/8 | 1/1 | M | DESC-10j, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10j | independent | DESC-09 | N/A |
| DESC-05 | implementation | misnamed tag 内部名与 `--abbrev=0` 强制后缀 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 8/8 | 3/8 | 1/2 | M | DESC-10k, DEP-DESC-541, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-01..10 | independent | N/A | N/A |
| DESC-04 | implementation | `--no-long` 标志与 last-wins 交互 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 7/8 | 5/8 | 1/1 | M | DESC-05, DEP-DESC-541, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-01..10 | independent | N/A | N/A |
| DESC-06 | implementation | `--no-abbrev` 等价 `--abbrev=0` 与 `--always` | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 8/8 | 3/8 | 1/2 | M | DESC-04, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-01..10 | independent | N/A | N/A |
| DESC-07 | implementation | 非 `--contains` 拒绝 `--dirty` 与 `<commit>` 共用 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 7/8 | 3/8 | 1/2 | M | DESC-06, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-01..10 | independent | N/A | N/A |
| DESC-08 | implementation | `--broken[=<mark>]` 支持与 `-dirty`/stat-only 回退 | 前滚新 patch / 降级 `PREV_VER` | yes | yes | 8/8 | 3/8 | 1/2 | M | DESC-07, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-01..10 | independent | N/A | N/A |

---

### Task DESC-01: 修复 `libra describe --contains` 在精确匹配附注 tag 时缺少 `^0` 后缀（#795）

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P1；Git 核心兼容缺陷

**Issue:** [#795](https://github.com/libra-tools/libra/issues/795)

**Description:** 修复 `run_describe_contains` 在目标提交直接被**附注 tag**指向时输出 `<tag>^0`（轻量 tag 仍输出 `<tag>`），并对齐 Git 在 `--contains` 路径不执行 `apply_dirty_mark` 的行为，同时更新既有 `describe_test.rs:687` 断言。

**Out of scope:** `--all` 下的 `--match` 过滤（由 `DESC-03` 承接）；嵌套 tag 多层剥皮（`#498 DEFER-TT-08`）；`--contains --exact-match` 与 Git 的既有差异（`DEFER-DESC-04`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `exact_match_only` 分支直接返回 `info.name.clone()` 并调用 `apply_dirty_mark` | `src/command/describe.rs:412-427` |
| 主循环 `generation == 0` 时 `display = base.clone()` 未对附注 tag 追加 `^0` 且调用 `apply_dirty_mark` | `src/command/describe.rs:464-496` |
| 现有测试在附注 tag 上断言 `--contains --exact-match` 返回 `"v1.0"` | `tests/command/describe_test.rs:687` |

**Acceptance criteria:**

- [ ] 无参数 `libra describe --contains`（默认 `HEAD`，`#795` 上游用例）在 `HEAD` 被附注 tag 直接指向时输出 `<tag>^0`。
- [ ] `libra describe --contains` 在目标被轻量 tag 直接指向时输出 `<tag>`（不加 `^0`）。
- [ ] `libra describe --contains --dirty` 不追加 `-dirty` 后缀（对齐 Git `describe.c:665-711`）。
- [ ] 更新 `tests/command/describe_test.rs:687`：`libra describe --contains --exact-match` 在附注 tag 精确匹配时输出 `<tag>^0`。
- [ ] 更新 `tests/command/describe_test.rs:687`：`libra describe --contains --candidates 0` 在附注 tag 精确匹配时输出 `<tag>^0`。
- [ ] `--json` 输出中 `data.result` 为 `<tag>^0`（附注 tag）。
- [ ] `--json` 输出中 `data.tag` 保持 `<tag>`（不含 `^0`）。
- [ ] `--json` 输出中 `data.exact_match` 在普通 `libra describe --contains` 精确命中附注 tag（主循环 `generation == 0`，基于 `generation == 0` 而非 `display == tag` 计算，`describe.rs:479`）时保持 `true`。
- [ ] ER-06a 强制门（不计入 G-03 上限）：`docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`、`COMPATIBILITY.md`（原地更新第 219 行）同步更新并逐文件验收。

**Verification:**

- [ ] `source .env.test && cargo test --test command_test describe_test::contains_exact_tag_appends_caret_zero` (new)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(describe_test::test_describe_contains) | test(describe_test::contains_exact_tag_appends_caret_zero)'`
- [ ] `cargo build --bin libra && TC_RUN_ID="desc-01-$(date -u +%Y%m%dT%H%M%SZ)-$$" LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1031-describe-contains/tc1031-describe-contains.sh`（TC-1031 / `t6120#27`，同步记录二进制 SHA-256 与 `HEAD` SHA）

**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）（仅改动 `src/command/describe.rs`、`tests/command/describe_test.rs` 与文档）

**Dependencies:** DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC

**Deliverables:** Inherited

**Implementation write set:** `src/command/describe.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Release write set:** Inherited

**Files likely touched:** `src/command/describe.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 需更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md` 与 `COMPATIBILITY.md`（第 219 行原地更新）中 `--contains` 对附注 tag 精确匹配输出 `<tag>^0` 的说明；写入后端文档前核验 `cf` 分支。

**Rollback mode:** immutable-release

**Estimated scope:** M（修改公开 CLI 输出行为）

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self

**Granularity:** `type=implementation; axis=--contains 附注 tag ^0 与忽略 --dirty; recovery=前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=1; scope=M; deps=DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-01..10; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-02: 为 `libra describe` 增加 `--no-match` 并支持重置 `--match` 过滤（#796）

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P1；Git CLI 选项兼容

**Issue:** [#796](https://github.com/libra-tools/libra/issues/796)

**Description:** 在 `DescribeArgs` 中增加 `--no-match` 标志，并通过 `overrides_with` 按命令行参数顺序实现重置语义（清除此前的所有 `--match` 模式，同时保留其后新传入的 `--match` 模式），同步更新 `describe_commit_for_atom` 字段初始化。

**Out of scope:** `--all` 分支过滤（由 `DESC-03` 承接）；`--no-exclude`（不在 `#796` 范围内，尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DescribeArgs` 仅定义了 `match_patterns: Vec<String>`，无 `--no-match` | `src/command/describe.rs:102-104` |
| `describe_commit_for_atom` 构造 `DescribeArgs` 字面量 | `src/command/describe.rs:159-173` |
| `compile_globs(&args.match_patterns)` 直接编译全部 `--match` | `src/command/describe.rs:216` |

**Acceptance criteria:**

- [ ] `libra describe --match 'v1.*' --no-match` 清空之前的 `--match` 过滤。
- [ ] `libra describe --match 'v1.*' --no-match --match 'v2.*'` 仅保留 `--no-match` 之后的 `v2.*`。
- [ ] `libra describe --contains --match=B --no-match`（`#796` / `t6120` 上游用例）正常命中 `A^0`。
- [ ] 单独传入 `--no-match` 合法且等效于未指定 `--match`。
- [ ] `--no-match` 与 `--exclude` 组合时仍保留 `--exclude` 优先级。
- [ ] ER-06a 强制门（不计入 G-03 上限）：`docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`、`COMPATIBILITY.md`（原地更新第 219 行）同步更新并逐文件验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(describe_test::no_match_resets_previous_match_patterns)'` (new)
- [ ] `source .env.test && cargo nextest run --lib -E 'test(command::describe::tests)'`
- [ ] `cargo build --bin libra && TC_RUN_ID="desc-02-$(date -u +%Y%m%dT%H%M%SZ)-$$" LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1034-describe-contains/tc1034-describe-contains.sh`（TC-1034 / `t6120#66`，同步记录二进制 SHA-256 与 `HEAD` SHA）

**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）

**Dependencies:** DESC-01, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC

**Deliverables:** Inherited

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Release write set:** Inherited

**Files likely touched:** `src/command/describe.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 需更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md` 与 `COMPATIBILITY.md`（第 219 行原地更新）补充 `--no-match` 选项说明。

**Rollback mode:** immutable-release

**Estimated scope:** M（新增 1 处公开 CLI 标志 `--no-match`）

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self

**Granularity:** `type=implementation; axis=--no-match 重置 --match 过滤; recovery=前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=1; prod-files=1; scope=M; deps=DESC-01,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-01..10; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-03: 修复 `libra describe --all` 忽略 `--match`/`--exclude` 过滤（#797）

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P1；Git 过滤语义缺陷

**Issue:** [#797](https://github.com/libra-tools/libra/issues/797)

**Description:** 使 `--all` 模式下收集的本地分支与远程分支同样经过 `tag_passes_filters`（`--match` / `--exclude`）过滤（非 `--contains` 匹配短分支名 `branch_x` / `origin/x`，`--contains --all` 匹配 `refs/{tags,heads,remotes}/<pat>`），无任何候选匹配时失败返回 `NoContainingTag` / `NoNamesFound`。

**Out of scope:** `--no-match` 参数解析（由 `DESC-02` 完成）；`--contains --all` 输出移除 `heads/` 前缀（`DEFER-DESC-03`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `all_tags` 循环调用 `tag_passes_filters(&t.name, &matchers, &excluders)` | `src/command/describe.rs:232-234` |
| `if include_all` 分支直接插入 `heads/...` 与 `remotes/...`，未调用过滤函数 | `src/command/describe.rs:265-289` |

**Acceptance criteria:**

- [ ] `libra describe --contains --all --match '<nonexistent>'` 在无任何 ref 匹配时返回 `NoContainingTag` 错误（覆盖 `#797` / `t6120` 场景）。
- [ ] `libra describe --all --match '<pattern>'`（非 `--contains`）按分支短名（不含 `heads/`/`remotes/` 前缀）执行 `--match` 过滤。
- [ ] `libra describe --all --exclude '<pattern>'`（非 `--contains`）按分支短名（不含 `heads/`/`remotes/` 前缀）执行 `--exclude` 过滤。
- [ ] `libra describe --contains --all --match '<pattern>'` 按完整 ref 命名空间（`refs/{tags,heads,remotes}/` 等价规则，对齐 Git `describe.c:680-694`）过滤候选。
- [ ] 当 tag 与分支均通过过滤并指向同一提交时，仍保持 Libra 现有 tag > heads > remotes 优先级。
- [ ] 仅 `--all`（无 `--match`/`--exclude`）的既有行为保持不变。
- [ ] ER-06a 强制门（不计入 G-03 上限）：`docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`、`COMPATIBILITY.md`（原地更新第 219 行）同步更新并逐文件验收。

**Verification:**

- [ ] `source .env.test && cargo test --test command_test describe_test::contains_all_respects_match_filter_and_fails_when_unmatched` (new)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(describe_test::test_describe_all) | test(describe_test::contains_all_respects_match_filter_and_fails_when_unmatched)'`
- [ ] `cargo build --bin libra && TC_RUN_ID="desc-03-$(date -u +%Y%m%dT%H%M%SZ)-$$" LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1035-describe-contains/tc1035-describe-contains.sh`（TC-1035 / `t6120#67`，同步记录二进制 SHA-256 与 `HEAD` SHA）

**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）

**Dependencies:** DESC-02, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC

**Deliverables:** Inherited

**Implementation write set:** `src/command/describe.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Release write set:** Inherited

**Files likely touched:** `src/command/describe.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 需更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md` 与 `COMPATIBILITY.md`（第 219 行原地更新）明确 `--match`/`--exclude` 同样作用于 `--all` 的 branches/remotes。

**Rollback mode:** immutable-release

**Estimated scope:** M（修改公开 CLI 过滤行为）

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self

**Granularity:** `type=implementation; axis=--all 候选受 --match/--exclude 过滤; recovery=前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=1; prod-files=1; scope=M; deps=DESC-02,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-01..10; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-09: 在 `rev-parse` 中支持 `<tag>-<N>-g<sha>` 格式解析（`#798`/`#799` 往返前置）

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P1；`#798` 与 `#799` `rev-parse` 往返验收前置

**Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 在 `resolve_object_atom_typed`（`src/utils/util.rs`）中增加 Git `describe` 输出格式 `<prefix>-<N>-g<hex>` 的回退解析：优先按完整引用名解析，未命中且末尾匹配 `-<digits>-g<hex>`（`hex` 至少 4 位十六进制）时解析 commit 的缩写或全长对象哈希；全长和缩写、对象不存在及歧义行为先用目标 Git 版本对照固定，保持无关纯哈希表达式既有行为不变。缺失或歧义对象在本卡保持 fail-closed，具体区分缺失与歧义错误由 `DESC-09b` 扩展；非提交对象在本卡暂时明确拒绝，由 `DESC-09a` 扩展 blob/tree；文档标明暂时边界。

**Out of scope:** `tags/`/`heads/`/`remotes/` 及 `refs/tags/`、`refs/heads/`、`refs/remotes/` 命名空间前缀回退（按 G-09 从本卡拆出 `DESC-10` 承接，见 `DESC-10` `split-from=DESC-09`）；`describe` 本身的 tag/输出逻辑（由 `DESC-05`/`DESC-04` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_object_atom_typed` 仅尝试 ref 查找与纯十六进制前缀查找，且 `tags/`/`heads/`/`remotes/` 前缀未命中时直接返回错误 | `src/utils/util.rs:2319-2335, 2404-2468` |

**Acceptance criteria:**

- [ ] 未命中字面引用时，缩写 commit OID 后缀与 Git 对照解析一致。
- [ ] 未命中字面引用时，全长 commit OID 后缀与 Git 对照解析一致。
- [ ] 存在同名且后缀符合语法的分支时优先解析该字面分支。
- [ ] 存在同名且后缀符合语法的 tag 时优先解析该字面 tag。
- [ ] 后缀语法无效的合法同名分支优先于后缀语法检查。
- [ ] 后缀语法无效的合法同名 tag 优先于后缀语法检查。
- [ ] 字面引用未命中且后缀不符合「至少四位十六进制」时返回无效引用错误。
- [ ] ER-06a 强制门（不计入 G-03 上限）：`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`（原地更新 `:217` `rev-parse` 行）同步更新并逐文件验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_describe_output_suffix)'` (new；覆盖 commit 的全长与缩写 OID、错误路径及非提交对象暂时拒绝)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_rev_parse_describe_output_suffix)'` (new；覆盖 commit 的全长与缩写 OID、有效和无效后缀字面分支/tag 优先及错误路径；非提交对象在本卡暂时拒绝)

**Full-suite trigger:** T-1: 修改共享修订解析器 `src/utils/util.rs`（执行阶段及 C 组第 ④ 步按 ER-13 跑全量 nextest）

**Dependencies:** DESC-03, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT

**Deliverables:** Inherited

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`

**Release write set:** Inherited

**Files likely touched:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 同步更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 与 `COMPATIBILITY.md`（`:217` `rev-parse` 行原地更新，不增删行）补充 `<tag>-<N>-g<sha>` 语法说明；写入后端文档前核验 `cf` 分支。

**Rollback mode:** immutable-release

**Estimated scope:** M（扩展共享修订语法解析）

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self

**Granularity:** `type=implementation; axis=rev-parse 解析 <tag>-<N>-g<sha> 格式与 OID 校验; recovery=前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-03,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-01..10; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-09b: 后缀 OID 缺失与歧义错误处理

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-09` 字面引用优先与 commit 后缀解析基础上，对未命中字面引用的 OID 后缀明确校验存在性和唯一性；在本卡之前对无法唯一验证的对象保持 fail-closed，不改变已有合法引用优先行为。
**Out of scope:** 非提交对象支持（`DESC-09a`）及命名空间前缀（`DESC-10..10e`）。
**Current evidence:** `src/utils/util.rs:2319-2468`；实现前复现 Git 对缺失、歧义和全长对象的行为。
**Acceptance criteria:**
- [ ] 不存在的缩写 OID 返回标准无效引用错误。
- [ ] 不存在的全长 OID 返回标准无效引用错误。
- [ ] 歧义缩写 OID 返回标准歧义引用错误。
- [ ] ER-06a 同卡强制门：以下文件逐一同步并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_09b_invalid_oid)'`（new；确认至少一个测试运行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_09b_invalid_oid)'`（new；逐项覆盖三类错误并回归合法与无效后缀字面引用优先）
**Full-suite trigger:** T-1：修改共享修订解析器。
**Dependencies:** DESC-09, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐一验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`，说明对象缺失和歧义。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=后缀OID缺失与歧义错误; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-09,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-09; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-09c: 附注 tag 对象后缀解析

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-09b` 基础上扩展附注 tag 对象自身的 OID 后缀，不移除对象的 tag 类型语义；完整与缩写均须与目标 Git 对照。同步修改 DESC-09 中 tag 暂时拒绝断言为接受原 tag OID，blob/tree 拒绝断言保留；本卡复跑 DESC-09 的更新后测试。此行为在 Git 临时夹具中已对附注 tag OID 的完整和八位后缀确认，实装时再次固定 SHA-1/SHA-256 对照。
**Out of scope:** blob/tree 对象（`DESC-09a`）及命名空间前缀（`DESC-10..10e`）。
**Current evidence:** 临时 Git 仓库对附注 tag 对象 `fixture-tag-1-g<tag_oid>` 返回同一 tag OID，而非其指向的 commit。
**Acceptance criteria:**
- [ ] 附注 tag 对象的缩写 OID 后缀与目标 Git 解析一致。
- [ ] 附注 tag 对象的全长 OID 后缀与目标 Git 解析一致。
- [ ] ER-06a 同卡强制门：逐文件更新并验收下列文档及兼容矩阵。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_09c_tag_object)'`（new；确认至少运行一个测试）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_describe_output_suffix)'`（修改并复跑前序单测的临时拒绝迁移断言；至少运行一个测试）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_09c_tag_object)'`（new；分别对照完整与缩写 tag OID、其指向 commit 不得替换、字面引用优先及错误路径）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_rev_parse_describe_output_suffix)'`（更新前序测试；至少运行一个测试）
**Full-suite trigger:** T-1：修改共享修订解析器 `src/utils/util.rs`。
**Dependencies:** DESC-09b, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 逐文件更新并验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`，说明附注 tag 对象后缀。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=附注tag对象后缀解析; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=2/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=DESC-09b,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-09b; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-09a: 支持无命名空间后缀解析 blob 与 tree

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-09` 的字面引用优先及 OID 校验基础上扩展无命名空间 `<prefix>-<N>-g<sha>`，允许 Git 接受的 blob 和 tree 对象，移除对这两种类型的临时拒绝；保持已由 DESC-09c 交付的 tag 对象身份、commit 和错误路径不回归；同步修改 DESC-09 的 blob/tree 临时拒绝断言为接受对应原始 OID，运行更新后的前序测试。
**Out of scope:** 命名空间前缀回退（`DESC-10`）。
**Current evidence:** Git 对 blob 后缀可解析，见 R38-A；实装前重新对照目标 Git 与 SHA-1/SHA-256 仓库。
**Acceptance criteria:**
- [ ] 缩写 blob OID 后缀与 Git 解析结果一致。
- [ ] 全长 blob OID 后缀与 Git 解析结果一致。
- [ ] 缩写 tree OID 后缀与 Git 解析结果一致。
- [ ] 全长 tree OID 后缀与 Git 解析结果一致。
- [ ] ER-06a 同卡强制门：逐文件同步并验收下列文档和兼容矩阵。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_describe_noncommit_suffix)'`（new，至少运行一个测试）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_describe_output_suffix)'`（修改并复跑前序单测的临时拒绝迁移断言；至少运行一个测试）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_rev_parse_describe_noncommit_suffix)'`(new，覆盖 blob/tree 四种对象与长度组合，回归 DESC-09c 的缩写/全长附注 tag OID 且断言返回 tag 对象自身而非 commit，并回归字面引用、commit、歧义和缺失对象)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_rev_parse_describe_output_suffix)'`（更新前序测试；至少运行一个测试）
**Full-suite trigger:** T-1：修改共享修订解析器 `src/utils/util.rs`。
**Dependencies:** DESC-09c, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 逐文件更新并验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 与 `COMPATIBILITY.md`，移除非提交对象临时拒绝的限制。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=无命名空间blob与tree后缀解析; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=DESC-09c,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-09; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-10: 支持 `tags/` describe 后缀回退

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-09a` 基础上仅为 `tags/` 添加字面引用未命中时的 `<prefix>-<N>-g<hex>` 对象回退，复用 DESC-09a 的对象存在性与歧义处理；字面引用优先。本卡暂不支持 tag 对象后缀，必须 fail-closed；该前缀 tag 支持由 DESC-10f..10k 对应后继卡实现。未交付的其他前缀保持既有行为，在各自后继卡扩展并在文档标注边界。
**Out of scope:** 其他命名空间和 describe 输出生成。
**Current evidence:** `src/utils/util.rs:2404-2468` 的命名空间早返回；实现前对目标 Git 和 SHA-1/SHA-256 仓库复现。
**Acceptance criteria:**
- [ ] `tags/` 字面引用未命中时，缩写 commit OID 后缀的解析与目标 Git 一致。
- [ ] `tags/` 字面引用未命中时，全长 commit OID 后缀的解析与目标 Git 一致。
- [ ] `tags/` 字面引用未命中时，缩写 tree OID 后缀的解析与目标 Git 一致。
- [ ] `tags/` 字面引用未命中时，全长 tree OID 后缀的解析与目标 Git 一致。
- [ ] `tags/` 字面引用未命中时，缩写 blob OID 后缀的解析与目标 Git 一致。
- [ ] `tags/` 字面引用未命中时，全长 blob OID 后缀的解析与目标 Git 一致。
- [ ] `tags/` 同名字面引用优先于后缀回退。
- [ ] `tags/<tag>-<N>-g<sha>~1` 基于 commit 原子继续解析。
- [ ] ER-06a 同卡强制门：以下每个文档文件逐一更新并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_10_suffix)'`（new；至少运行一个测试）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_10_suffix)'`（new；覆盖 commit/tree/blob 六种对象/长度、tag 暂时拒绝、字面引用、缺失及歧义 OID，并逐项对照 Git；至少运行一个测试；显式验证 `tags/<tag>-<N>-g<sha>~1` 返回目标 commit 的预期父提交并与 Git 对照）
**Full-suite trigger:** T-1：修改共享修订解析器 `src/utils/util.rs`。
**Dependencies:** DESC-09a, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐文件验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`，说明 `tags/` 已支持与未交付前缀的边界。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=tags/命名空间后缀回退; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-09a,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-09a; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-10a: 支持 `heads/` describe 后缀回退

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-10` 基础上仅为 `heads/` 添加字面引用未命中时的 `<prefix>-<N>-g<hex>` 对象回退，复用 DESC-09a 的对象存在性与歧义处理；字面引用优先。本卡暂不支持 tag 对象后缀，必须 fail-closed；该前缀 tag 支持由 DESC-10f..10k 对应后继卡实现。未交付的其他前缀保持既有行为，在各自后继卡扩展并在文档标注边界。
**Out of scope:** 其他命名空间和 describe 输出生成。
**Current evidence:** `src/utils/util.rs:2404-2468` 的命名空间早返回；实现前对目标 Git 和 SHA-1/SHA-256 仓库复现。
**Acceptance criteria:**
- [ ] `heads/` 字面引用未命中时，缩写 commit OID 后缀的解析与目标 Git 一致。
- [ ] `heads/` 字面引用未命中时，全长 commit OID 后缀的解析与目标 Git 一致。
- [ ] `heads/` 字面引用未命中时，缩写 tree OID 后缀的解析与目标 Git 一致。
- [ ] `heads/` 字面引用未命中时，全长 tree OID 后缀的解析与目标 Git 一致。
- [ ] `heads/` 字面引用未命中时，缩写 blob OID 后缀的解析与目标 Git 一致。
- [ ] `heads/` 字面引用未命中时，全长 blob OID 后缀的解析与目标 Git 一致。
- [ ] `heads/` 同名字面引用优先于后缀回退。
- [ ] ER-06a 同卡强制门：以下每个文档文件逐一更新并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_10a_suffix)'`（new；至少运行一个测试）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_10a_suffix)'`（new；覆盖 commit/tree/blob 六种对象/长度、tag 暂时拒绝、字面引用、缺失及歧义 OID，并逐项对照 Git；至少运行一个测试）
**Full-suite trigger:** T-1：修改共享修订解析器 `src/utils/util.rs`。
**Dependencies:** DESC-10, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐文件验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`，说明 `heads/` 已支持与未交付前缀的边界。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=heads/命名空间后缀回退; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-10,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-10; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-10b: 支持 `remotes/` describe 后缀回退

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-10a` 基础上仅为 `remotes/` 添加字面引用未命中时的 `<prefix>-<N>-g<hex>` 对象回退，复用 DESC-09a 的对象存在性与歧义处理；字面引用优先。本卡暂不支持 tag 对象后缀，必须 fail-closed；该前缀 tag 支持由 DESC-10f..10k 对应后继卡实现。未交付的其他前缀保持既有行为，在各自后继卡扩展并在文档标注边界。
**Out of scope:** 其他命名空间和 describe 输出生成。
**Current evidence:** `src/utils/util.rs:2404-2468` 的命名空间早返回；实现前对目标 Git 和 SHA-1/SHA-256 仓库复现。
**Acceptance criteria:**
- [ ] `remotes/` 字面引用未命中时，缩写 commit OID 后缀的解析与目标 Git 一致。
- [ ] `remotes/` 字面引用未命中时，全长 commit OID 后缀的解析与目标 Git 一致。
- [ ] `remotes/` 字面引用未命中时，缩写 tree OID 后缀的解析与目标 Git 一致。
- [ ] `remotes/` 字面引用未命中时，全长 tree OID 后缀的解析与目标 Git 一致。
- [ ] `remotes/` 字面引用未命中时，缩写 blob OID 后缀的解析与目标 Git 一致。
- [ ] `remotes/` 字面引用未命中时，全长 blob OID 后缀的解析与目标 Git 一致。
- [ ] `remotes/` 同名字面引用优先于后缀回退。
- [ ] ER-06a 同卡强制门：以下每个文档文件逐一更新并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_10b_suffix)'`（new；至少运行一个测试）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_10b_suffix)'`（new；覆盖 commit/tree/blob 六种对象/长度、tag 暂时拒绝、字面引用、缺失及歧义 OID，并逐项对照 Git；至少运行一个测试）
**Full-suite trigger:** T-1：修改共享修订解析器 `src/utils/util.rs`。
**Dependencies:** DESC-10a, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐文件验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`，说明 `remotes/` 已支持与未交付前缀的边界。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=remotes/命名空间后缀回退; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-10a,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-10a; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-10c: 支持 `refs/tags/` describe 后缀回退

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-10b` 基础上仅为 `refs/tags/` 添加字面引用未命中时的 `<prefix>-<N>-g<hex>` 对象回退，复用 DESC-09a 的对象存在性与歧义处理；字面引用优先。本卡暂不支持 tag 对象后缀，必须 fail-closed；该前缀 tag 支持由 DESC-10f..10k 对应后继卡实现。未交付的其他前缀保持既有行为，在各自后继卡扩展并在文档标注边界。
**Out of scope:** 其他命名空间和 describe 输出生成。
**Current evidence:** `src/utils/util.rs:2404-2468` 的命名空间早返回；实现前对目标 Git 和 SHA-1/SHA-256 仓库复现。
**Acceptance criteria:**
- [ ] `refs/tags/` 字面引用未命中时，缩写 commit OID 后缀的解析与目标 Git 一致。
- [ ] `refs/tags/` 字面引用未命中时，全长 commit OID 后缀的解析与目标 Git 一致。
- [ ] `refs/tags/` 字面引用未命中时，缩写 tree OID 后缀的解析与目标 Git 一致。
- [ ] `refs/tags/` 字面引用未命中时，全长 tree OID 后缀的解析与目标 Git 一致。
- [ ] `refs/tags/` 字面引用未命中时，缩写 blob OID 后缀的解析与目标 Git 一致。
- [ ] `refs/tags/` 字面引用未命中时，全长 blob OID 后缀的解析与目标 Git 一致。
- [ ] `refs/tags/` 同名字面引用优先于后缀回退。
- [ ] ER-06a 同卡强制门：以下每个文档文件逐一更新并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_10c_suffix)'`（new；至少运行一个测试）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_10c_suffix)'`（new；覆盖 commit/tree/blob 六种对象/长度、tag 暂时拒绝、字面引用、缺失及歧义 OID，并逐项对照 Git；至少运行一个测试）
**Full-suite trigger:** T-1：修改共享修订解析器 `src/utils/util.rs`。
**Dependencies:** DESC-10b, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐文件验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`，说明 `refs/tags/` 已支持与未交付前缀的边界。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=refs/tags/命名空间后缀回退; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-10b,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-10b; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-10d: 支持 `refs/heads/` describe 后缀回退

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-10c` 基础上仅为 `refs/heads/` 添加字面引用未命中时的 `<prefix>-<N>-g<hex>` 对象回退，复用 DESC-09a 的对象存在性与歧义处理；字面引用优先。本卡暂不支持 tag 对象后缀，必须 fail-closed；该前缀 tag 支持由 DESC-10f..10k 对应后继卡实现。未交付的其他前缀保持既有行为，在各自后继卡扩展并在文档标注边界。
**Out of scope:** 其他命名空间和 describe 输出生成。
**Current evidence:** `src/utils/util.rs:2404-2468` 的命名空间早返回；实现前对目标 Git 和 SHA-1/SHA-256 仓库复现。
**Acceptance criteria:**
- [ ] `refs/heads/` 字面引用未命中时，缩写 commit OID 后缀的解析与目标 Git 一致。
- [ ] `refs/heads/` 字面引用未命中时，全长 commit OID 后缀的解析与目标 Git 一致。
- [ ] `refs/heads/` 字面引用未命中时，缩写 tree OID 后缀的解析与目标 Git 一致。
- [ ] `refs/heads/` 字面引用未命中时，全长 tree OID 后缀的解析与目标 Git 一致。
- [ ] `refs/heads/` 字面引用未命中时，缩写 blob OID 后缀的解析与目标 Git 一致。
- [ ] `refs/heads/` 字面引用未命中时，全长 blob OID 后缀的解析与目标 Git 一致。
- [ ] `refs/heads/` 同名字面引用优先于后缀回退。
- [ ] ER-06a 同卡强制门：以下每个文档文件逐一更新并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_10d_suffix)'`（new；至少运行一个测试）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_10d_suffix)'`（new；覆盖 commit/tree/blob 六种对象/长度、tag 暂时拒绝、字面引用、缺失及歧义 OID，并逐项对照 Git；至少运行一个测试）
**Full-suite trigger:** T-1：修改共享修订解析器 `src/utils/util.rs`。
**Dependencies:** DESC-10c, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐文件验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`，说明 `refs/heads/` 已支持与未交付前缀的边界。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=refs/heads/命名空间后缀回退; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-10c,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-10c; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-10e: 支持 `refs/remotes/` describe 后缀回退

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-10d` 基础上仅为 `refs/remotes/` 添加字面引用未命中时的 `<prefix>-<N>-g<hex>` 对象回退，复用 DESC-09a 的对象存在性与歧义处理；字面引用优先。本卡暂不支持 tag 对象后缀，必须 fail-closed；该前缀 tag 支持由 DESC-10f..10k 对应后继卡实现。未交付的其他前缀保持既有行为，在各自后继卡扩展并在文档标注边界。
**Out of scope:** 其他命名空间和 describe 输出生成。
**Current evidence:** `src/utils/util.rs:2404-2468` 的命名空间早返回；实现前对目标 Git 和 SHA-1/SHA-256 仓库复现。
**Acceptance criteria:**
- [ ] `refs/remotes/` 字面引用未命中时，缩写 commit OID 后缀的解析与目标 Git 一致。
- [ ] `refs/remotes/` 字面引用未命中时，全长 commit OID 后缀的解析与目标 Git 一致。
- [ ] `refs/remotes/` 字面引用未命中时，缩写 tree OID 后缀的解析与目标 Git 一致。
- [ ] `refs/remotes/` 字面引用未命中时，全长 tree OID 后缀的解析与目标 Git 一致。
- [ ] `refs/remotes/` 字面引用未命中时，缩写 blob OID 后缀的解析与目标 Git 一致。
- [ ] `refs/remotes/` 字面引用未命中时，全长 blob OID 后缀的解析与目标 Git 一致。
- [ ] `refs/remotes/` 同名字面引用优先于后缀回退。
- [ ] ER-06a 同卡强制门：以下每个文档文件逐一更新并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_10e_suffix)'`（new；至少运行一个测试）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_10e_suffix)'`（new；覆盖 commit/tree/blob 六种对象/长度、tag 暂时拒绝、字面引用、缺失及歧义 OID，并逐项对照 Git；至少运行一个测试）
**Full-suite trigger:** T-1：修改共享修订解析器 `src/utils/util.rs`。
**Dependencies:** DESC-10d, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐文件验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`，说明 `refs/remotes/` 已支持与未交付前缀的边界。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=refs/remotes/命名空间后缀回退; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-10d,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-10d; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-10f: `tags/` 附注 tag 对象后缀回退

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-10e` 基础上为 `tags/` 的 describe 后缀增加附注 tag 对象 OID 回退，保留字面引用优先和原始 tag 对象身份，不剥皮为 commit；其他未交付的 tag 前缀继续明确 fail-closed 并在文档标明临时边界。
**Out of scope:** 其他命名空间的 tag 对象扩展。
**Current evidence:** 临时 Git 仓库中 `tags/` 与 `refs/tags/` 的缩写 tag 对象后缀返回原 tag OID；其余前缀和全长须在开工时与目标 Git、SHA-1/SHA-256 夹具核对。
**Acceptance criteria:**
- [ ] `tags/` 的缩写 tag 对象 OID 后缀返回 tag 对象本身，并与目标 Git 一致。
- [ ] `tags/` 的全长 tag 对象 OID 后缀返回 tag 对象本身，并与目标 Git 一致。
- [ ] ER-06a 同卡强制门：下列文档和兼容矩阵逐文件更新并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_10f_tag_suffix)'`（new；至少一个测试运行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_10f_tag_suffix)'`（new；对照 Git 验证完整及缩写 OID 返回 tag 本身，回归 commit/tree/blob、字面引用优先与缺失/歧义）
**Full-suite trigger:** T-1：修改共享修订解析器。
**Dependencies:** DESC-10e, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐文件验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`；说明 `tags/` tag OID 支持和未交付边界。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=tags/附注tag对象后缀; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-10e,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-10e; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-10g: `heads/` 附注 tag 对象后缀回退

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-10f` 基础上为 `heads/` 的 describe 后缀增加附注 tag 对象 OID 回退，保留字面引用优先和原始 tag 对象身份，不剥皮为 commit；其他未交付的 tag 前缀继续明确 fail-closed 并在文档标明临时边界。
**Out of scope:** 其他命名空间的 tag 对象扩展。
**Current evidence:** 临时 Git 仓库中 `tags/` 与 `refs/tags/` 的缩写 tag 对象后缀返回原 tag OID；其余前缀和全长须在开工时与目标 Git、SHA-1/SHA-256 夹具核对。
**Acceptance criteria:**
- [ ] `heads/` 的缩写 tag 对象 OID 后缀返回 tag 对象本身，并与目标 Git 一致。
- [ ] `heads/` 的全长 tag 对象 OID 后缀返回 tag 对象本身，并与目标 Git 一致。
- [ ] ER-06a 同卡强制门：下列文档和兼容矩阵逐文件更新并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_10g_tag_suffix)'`（new；至少一个测试运行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_10g_tag_suffix)'`（new；对照 Git 验证完整及缩写 OID 返回 tag 本身，回归 commit/tree/blob、字面引用优先与缺失/歧义）
**Full-suite trigger:** T-1：修改共享修订解析器。
**Dependencies:** DESC-10f, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐文件验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`；说明 `heads/` tag OID 支持和未交付边界。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=heads/附注tag对象后缀; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-10f,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-10f; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-10h: `remotes/` 附注 tag 对象后缀回退

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-10g` 基础上为 `remotes/` 的 describe 后缀增加附注 tag 对象 OID 回退，保留字面引用优先和原始 tag 对象身份，不剥皮为 commit；其他未交付的 tag 前缀继续明确 fail-closed 并在文档标明临时边界。
**Out of scope:** 其他命名空间的 tag 对象扩展。
**Current evidence:** 临时 Git 仓库中 `tags/` 与 `refs/tags/` 的缩写 tag 对象后缀返回原 tag OID；其余前缀和全长须在开工时与目标 Git、SHA-1/SHA-256 夹具核对。
**Acceptance criteria:**
- [ ] `remotes/` 的缩写 tag 对象 OID 后缀返回 tag 对象本身，并与目标 Git 一致。
- [ ] `remotes/` 的全长 tag 对象 OID 后缀返回 tag 对象本身，并与目标 Git 一致。
- [ ] ER-06a 同卡强制门：下列文档和兼容矩阵逐文件更新并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_10h_tag_suffix)'`（new；至少一个测试运行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_10h_tag_suffix)'`（new；对照 Git 验证完整及缩写 OID 返回 tag 本身，回归 commit/tree/blob、字面引用优先与缺失/歧义）
**Full-suite trigger:** T-1：修改共享修订解析器。
**Dependencies:** DESC-10g, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐文件验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`；说明 `remotes/` tag OID 支持和未交付边界。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=remotes/附注tag对象后缀; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-10g,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-10g; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-10i: `refs/tags/` 附注 tag 对象后缀回退

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-10h` 基础上为 `refs/tags/` 的 describe 后缀增加附注 tag 对象 OID 回退，保留字面引用优先和原始 tag 对象身份，不剥皮为 commit；其他未交付的 tag 前缀继续明确 fail-closed 并在文档标明临时边界。
**Out of scope:** 其他命名空间的 tag 对象扩展。
**Current evidence:** 临时 Git 仓库中 `tags/` 与 `refs/tags/` 的缩写 tag 对象后缀返回原 tag OID；其余前缀和全长须在开工时与目标 Git、SHA-1/SHA-256 夹具核对。
**Acceptance criteria:**
- [ ] `refs/tags/` 的缩写 tag 对象 OID 后缀返回 tag 对象本身，并与目标 Git 一致。
- [ ] `refs/tags/` 的全长 tag 对象 OID 后缀返回 tag 对象本身，并与目标 Git 一致。
- [ ] ER-06a 同卡强制门：下列文档和兼容矩阵逐文件更新并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_10i_tag_suffix)'`（new；至少一个测试运行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_10i_tag_suffix)'`（new；对照 Git 验证完整及缩写 OID 返回 tag 本身，回归 commit/tree/blob、字面引用优先与缺失/歧义）
**Full-suite trigger:** T-1：修改共享修订解析器。
**Dependencies:** DESC-10h, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐文件验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`；说明 `refs/tags/` tag OID 支持和未交付边界。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=refs/tags/附注tag对象后缀; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-10h,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-10h; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-10j: `refs/heads/` 附注 tag 对象后缀回退

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-10i` 基础上为 `refs/heads/` 的 describe 后缀增加附注 tag 对象 OID 回退，保留字面引用优先和原始 tag 对象身份，不剥皮为 commit；其他未交付的 tag 前缀继续明确 fail-closed 并在文档标明临时边界。
**Out of scope:** 其他命名空间的 tag 对象扩展。
**Current evidence:** 临时 Git 仓库中 `tags/` 与 `refs/tags/` 的缩写 tag 对象后缀返回原 tag OID；其余前缀和全长须在开工时与目标 Git、SHA-1/SHA-256 夹具核对。
**Acceptance criteria:**
- [ ] `refs/heads/` 的缩写 tag 对象 OID 后缀返回 tag 对象本身，并与目标 Git 一致。
- [ ] `refs/heads/` 的全长 tag 对象 OID 后缀返回 tag 对象本身，并与目标 Git 一致。
- [ ] ER-06a 同卡强制门：下列文档和兼容矩阵逐文件更新并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_10j_tag_suffix)'`（new；至少一个测试运行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_10j_tag_suffix)'`（new；对照 Git 验证完整及缩写 OID 返回 tag 本身，回归 commit/tree/blob、字面引用优先与缺失/歧义）
**Full-suite trigger:** T-1：修改共享修订解析器。
**Dependencies:** DESC-10i, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐文件验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`；说明 `refs/heads/` tag OID 支持和未交付边界。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=refs/heads/附注tag对象后缀; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-10i,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-10i; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-10k: `refs/remotes/` 附注 tag 对象后缀回退

**Task type:** implementation
**Lifecycle / Acceptance:** pending / 空
**Description:** 在 `DESC-10j` 基础上为 `refs/remotes/` 的 describe 后缀增加附注 tag 对象 OID 回退，保留字面引用优先和原始 tag 对象身份，不剥皮为 commit；其他未交付的 tag 前缀继续明确 fail-closed 并在文档标明临时边界。
**Out of scope:** 其他命名空间的 tag 对象扩展。
**Current evidence:** 临时 Git 仓库中 `tags/` 与 `refs/tags/` 的缩写 tag 对象后缀返回原 tag OID；其余前缀和全长须在开工时与目标 Git、SHA-1/SHA-256 夹具核对。
**Acceptance criteria:**
- [ ] `refs/remotes/` 的缩写 tag 对象 OID 后缀返回 tag 对象本身，并与目标 Git 一致。
- [ ] `refs/remotes/` 的全长 tag 对象 OID 后缀返回 tag 对象本身，并与目标 Git 一致。
- [ ] ER-06a 同卡强制门：下列文档和兼容矩阵逐文件更新并验收。
**Verification:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(utils::util::test::resolve_desc_10k_tag_suffix)'`（new；至少一个测试运行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(rev_parse_test::test_desc_10k_tag_suffix)'`（new；对照 Git 验证完整及缩写 OID 返回 tag 本身，回归 commit/tree/blob、字面引用优先与缺失/歧义）
**Full-suite trigger:** T-1：修改共享修订解析器。
**Dependencies:** DESC-10j, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT
**Deliverables:** Inherited
**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `COMPATIBILITY.md`
**Release write set:** Inherited
**Files likely touched:** 同 Implementation write set。
**Docs and compatibility impact:** 更新并逐文件验收 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`COMPATIBILITY.md`；说明 `refs/remotes/` tag OID 支持和未交付边界。
**Rollback mode:** immutable-release
**Estimated scope:** M
**Version increment:** patch
**Release boundary:** independent
**C/D coverage from:** self
**Granularity:** `type=implementation; axis=refs/remotes/附注tag对象后缀; recovery=前滚新patch或降级; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-10j,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=串行于DESC-10j; release=independent; split-from=DESC-09; exception=N/A`

---

### Task DESC-05: 支持 misnamed annotated tag 内部名称与 `--abbrev=0` 强制后缀输出（#799）

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P1；`#799` 修复与 `#798` 前置

**Issue:** [#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 当附注 tag 引用名（如 `refs/tags/Q`）与其指向的 tag 对象内部 `tag_name`（如 `A`）不一致时，在 `TagInfo` 中记录内部 `tag_name`（`--all` 下为 `tags/<tag_name>`）与 `misnamed = true` 标志，向 stderr 打印 Git 警告，并在格式输出中强制追加 `-<distance>-g<sha>` 后缀（`--abbrev=0` 时输出完整 SHA），关闭 `#799`。

**Out of scope:** 修改 `libra update-ref` 支持 `refs/tags/*`（测试通过 `libra tag Q <tag-oid>` + `libra tag -d A` 构造夹具）；`--no-long` 选项（由 `DESC-04` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `TagInfo` 仅存储 `t.name` 与 `is_annotated`，未使用 `TagObject::Tag(tg).tag_name` | `src/command/describe.rs:118-121, 225-258` |
| `describe_output` / `format_describe_result` 对 `abbrev == 0` 置 `abbreviated_commit: None` 且省略后缀 | `src/command/describe_format.rs:11-50` |
| 现有测试断言常规 tag `--abbrev=0` 输出 `"v1.0"` | `tests/command/describe_test.rs:113-135` |

**Acceptance criteria:**

- [ ] 非 `--contains` 路径下，当 `t.name != tg.tag_name` 时使用 `tg.tag_name` 作为展示名。
- [ ] 仅对最终选定输出的 misnamed tag 向 stderr 打印警告 `warning: tag '<ref>' is externally known as '<tag_name>'`。
- [ ] `misnamed == true` 且 `--abbrev=0`（`t6120` 的 `Q`→`A` 场景，描述子提交 `c`）输出 `A-1-g<full-sha>`。
- [ ] 上述 `A-1-g<full-sha>` 可由 `libra rev-parse` 解析回目标提交。
- [ ] `misnamed == true` 且 `--abbrev=0 --json` 输出 `data.tag == "A"`。
- [ ] `misnamed == true` 且 `--abbrev=0 --json` 输出 `data.abbreviated_commit` 为完整 SHA 字符串。
- [ ] `--contains` 路径保持使用引用名 `t.name`（输出 `Q^0`，对齐 Git `describe.c:665-711`）。
- [ ] `--contains` 路径在 misnamed tag 上不向 stderr 打印 misnamed 警告。
- [ ] ER-06a 强制门（不计入 G-03 上限）：`docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`、`COMPATIBILITY.md`（原地更新第 219 行）同步更新并逐文件验收。

**Verification:**

- [ ] `source .env.test && cargo test --test command_test describe_test::misnamed_annotated_tag_abbrev_zero_forces_full_suffix` (new，含 stderr 警告与 `rev-parse` 往返断言)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(describe_test::test_describe_abbrev_zero) | test(describe_test::test_describe_always_abbrev_zero) | test(describe_test::misnamed_annotated_tag_abbrev_zero_forces_full_suffix)'`
- [ ] `cargo build --bin libra && TC_RUN_ID="desc-05-$(date -u +%Y%m%dT%H%M%SZ)-$$" LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1045-describe-abbrev/tc1045-describe-abbrev.sh`（TC-1045 / `t6120#32`，同步记录二进制 SHA-256 与 `HEAD` SHA）

**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）

**Dependencies:** DESC-10k, DEP-DESC-541, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC

**Deliverables:** Inherited

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_format.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Release write set:** Inherited

**Files likely touched:** `src/command/describe.rs`, `src/command/describe_format.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 同步在 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md` 与 `COMPATIBILITY.md`（第 219 行原地更新）说明 misnamed annotated tag 行为。

**Rollback mode:** immutable-release

**Estimated scope:** M（修改公开 CLI 输出与警告行为）

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self

**Granularity:** `type=implementation; axis=misnamed tag 内部名与 --abbrev=0 强制后缀; recovery=前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=DESC-10k,DEP-DESC-541,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-01..10; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-04: 为 `libra describe` 增加 `--no-long` 并支持 last-wins 覆盖（#798）

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P1；Git CLI 选项兼容

**Issue:** [#798](https://github.com/libra-tools/libra/issues/798)

**Description:** 在 `DescribeArgs` 中增加 `--no-long` 标志，使 `--long` 与 `--no-long` 遵循命令行 last-wins 覆盖，并验证在 misnamed tag 上 `--no-long` 仍输出 `A-0-g<sha>` 且可由 `libra rev-parse` 解析回目标提交（依赖 `DESC-05`）。

**Out of scope:** `--no-abbrev`（由 `DESC-06` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DescribeArgs` 仅定义了 `#[clap(long)] pub long: bool` | `src/command/describe.rs:90-92` |
| `describe_commit_for_atom` 构造 `DescribeArgs` 字面量 | `src/command/describe.rs:159-173` |
| `long_format && abbrev == 0` 校验逻辑 | `src/command/describe.rs:195-198` |

**Acceptance criteria:**

- [ ] `libra describe --no-long` 在正常精确 tag 匹配时输出 `<tag>`。
- [ ] `libra describe --long --no-long` 由 `--no-long` 覆盖并输出 `<tag>`。
- [ ] `libra describe --long --no-long --json` 中 `data.long_format == false`。
- [ ] `libra describe --no-long --long` 由 `--long` 覆盖并输出 `<tag>-0-g<hash>`。
- [ ] `libra describe --long --no-long --abbrev=0` 不触发 `LongWithAbbrevZero` 错误。
- [ ] `libra describe --no-long <misnamed-tag>`（复用 `DESC-05` 夹具）输出 `A-0-g<sha>`。
- [ ] 上述 `A-0-g<sha>` 可由 `libra rev-parse` 解析回目标提交（关闭 `#798` / `t6120#30`）。
- [ ] ER-06a 强制门（不计入 G-03 上限）：`docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`、`COMPATIBILITY.md`（原地更新第 219 行）同步更新并逐文件验收。

**Verification:**

- [ ] `source .env.test && cargo test --test command_test describe_long_test::test_describe_no_long_overrides_long_and_allows_abbrev_zero` (new)
- [ ] `source .env.test && cargo test --test command_test describe_test::misnamed_annotated_tag_no_long_keeps_suffix` (new)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(describe_long_test) | test(describe_test::misnamed_annotated_tag_no_long_keeps_suffix)'`
- [ ] `source .env.test && cargo nextest run --lib -E 'test(command::describe::tests)'`
- [ ] `cargo build --bin libra && TC_RUN_ID="desc-04-$(date -u +%Y%m%dT%H%M%SZ)-$$" LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1043-describe-long/tc1043-describe-long.sh`（TC-1043 / `t6120#30`，同步记录二进制 SHA-256 与 `HEAD` SHA）

**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）

**Dependencies:** DESC-05, DEP-DESC-541, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC

**Deliverables:** Inherited

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_tests.rs`, `tests/command/describe_long_test.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Release write set:** Inherited

**Files likely touched:** `src/command/describe.rs`, `src/command/describe_tests.rs`, `tests/command/describe_long_test.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 需更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md` 与 `COMPATIBILITY.md`（第 219 行原地更新）补充 `--no-long`。

**Rollback mode:** immutable-release

**Estimated scope:** M（新增 1 处公开 CLI 标志 `--no-long`）

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self

**Granularity:** `type=implementation; axis=--no-long 标志与 last-wins 交互; recovery=前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=7/8; VER=5/8; landing=1; prod-files=1; scope=M; deps=DESC-05,DEP-DESC-541,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-01..10; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-06: 为 `libra describe` 增加 `--no-abbrev`（等价 `--abbrev=0`）（#800）

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P1；Git CLI 选项兼容

**Issue:** [#800](https://github.com/libra-tools/libra/issues/800)

**Description:** 在 `DescribeArgs` 中增加 `--no-abbrev` 标志（Git `OPT__ABBREV` 中等价于 `--abbrev=0`），与 `--abbrev <N>` 遵循 last-wins 语义：常规 tag 匹配时省略 `-N-g<hash>` 后缀（misnamed tag 除外），`--always` 回退时输出完整 commit SHA（含 `--match=no-such-tag` 无匹配场景，不误报 `HEAD does not point to a commit`），且 `--long --no-abbrev` 拒绝报错。

**Out of scope:** `--dirty` 与 `<commit>` 互斥校验（由 `DESC-07` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DescribeArgs` 仅定义了 `--abbrev <N>`，无 `--no-abbrev` | `src/command/describe.rs:74-76` |
| `describe_commit_for_atom` 构造 `DescribeArgs` 字面量 | `src/command/describe.rs:159-173` |
| `long_format && abbrev == 0` 返回 `LongWithAbbrevZero` | `src/command/describe.rs:195-198` |
| `--always` 回退分支调用 `abbreviate_hash(&resolved_commit, abbrev)` | `src/command/describe.rs:362-378` |

**Acceptance criteria:**

- [ ] `libra describe --no-abbrev --always --match=no-such-tag`（`#800` 上游用例，存在 tag 但全部被过滤）输出完整 40/64 位 commit SHA。
- [ ] `libra describe --no-abbrev --always --match=no-such-tag` 不报 `HEAD does not point to a commit`。
- [ ] `libra describe --no-abbrev --always --json` 中 `data.used_always == true`。
- [ ] `libra describe --no-abbrev --always --json` 中 `data.abbreviated_commit == data.resolved_commit`。
- [ ] `libra describe --no-abbrev` 在常规可达 tag 存在时省略 `-N-g<hash>` 仅输出 `<tag>`。
- [ ] `--abbrev 5 --no-abbrev` 按 last-wins 生效（等效 `--abbrev=0`）。
- [ ] `--no-abbrev --abbrev 5` 按 last-wins 生效（输出 5 位缩写）。
- [ ] `libra describe --long --no-abbrev` 返回 `LongWithAbbrevZero` 错误（`LBR-CLI-002`）。
- [ ] ER-06a 强制门（不计入 G-03 上限）：`docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`、`COMPATIBILITY.md`（原地更新第 219 行）同步更新并逐文件验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(describe_test::no_abbrev_with_always_outputs_full_sha) | test(describe_test::no_abbrev_tag_and_long_conflict)'` (new)
- [ ] `source .env.test && cargo nextest run --lib -E 'test(command::describe::tests)'`
- [ ] `cargo build --bin libra && TC_RUN_ID="desc-06-$(date -u +%Y%m%dT%H%M%SZ)-$$" LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1046-describe-abbrev/tc1046-describe-abbrev.sh`（TC-1046 / `t6120#110`，同步记录二进制 SHA-256 与 `HEAD` SHA）

**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）

**Dependencies:** DESC-04, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC

**Deliverables:** Inherited

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_format.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Release write set:** Inherited

**Files likely touched:** `src/command/describe.rs`, `src/command/describe_format.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 需更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md` 与 `COMPATIBILITY.md`（第 219 行原地更新）补充 `--no-abbrev`（等价 `--abbrev=0`）。

**Rollback mode:** immutable-release

**Estimated scope:** M（新增 1 处公开 CLI 标志 `--no-abbrev`）

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self

**Granularity:** `type=implementation; axis=--no-abbrev 等价 --abbrev=0 与 --always; recovery=前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=DESC-04,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-01..10; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-07: 非 `--contains` 路径拒绝 `--dirty` 与 `<commit>` 共用（#802）

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P1；错误路径与参数校验

**Issue:** [#802](https://github.com/libra-tools/libra/issues/802)

**Description:** 在 `run_describe` 中（`args.contains` 分支之后）校验当 `dirty_mark.is_some()` 且显式传入 `<commit>` 参数时，fail-closed 返回专有错误（`LBR-CLI-002`，退出码 129）并给出 `--dirty` 与 commit-ish 不兼容的提示（不再复用 `--match/--exclude` glob 提示）。

**Out of scope:** `--broken` 选项实现（由 `DESC-08` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `args.commit` 与 `args.dirty` 未做互斥校验 | `src/command/describe.rs:62, 94-96, 189-209` |
| `InvalidArgument` 的提示固定指向 `--match/--exclude` glob | `src/command/describe.rs:661-663`, `src/command/describe_types.rs:40-41` |

**Acceptance criteria:**

- [ ] `libra describe --dirty HEAD` 返回退出码 129。
- [ ] `libra describe --dirty=-mk HEAD~1` 返回退出码 129。
- [ ] 人类可读错误提示明确指出 `--dirty` 与 commit-ish 不兼容。
- [ ] 该错误提示不输出 `--match`/`--exclude` glob 语法提示。
- [ ] `--json` 错误输出返回 `LBR-CLI-002` 结构化错误。
- [ ] `libra describe --contains --dirty HEAD` 保持被接受（对齐 Git `--contains` 先行返回语义）。
- [ ] `libra describe --dirty`（未传 `<commit>`）继续正常描述 `HEAD` 并检查工作区。
- [ ] ER-06a 强制门（不计入 G-03 上限）：`docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`、`COMPATIBILITY.md`（原地更新第 219 行）同步更新并逐文件验收。

**Verification:**

- [ ] `source .env.test && cargo test --test command_test describe_test::dirty_with_commit_arg_is_rejected` (new)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(describe_test::test_describe_dirty) | test(describe_test::dirty_with_commit_arg_is_rejected)'`
- [ ] `cargo build --bin libra && TC_RUN_ID="desc-07-$(date -u +%Y%m%dT%H%M%SZ)-$$" LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1052-describe-dirty/tc1052-describe-dirty.sh`（TC-1052 / `t6120#42`，同步记录二进制 SHA-256 与 `HEAD` SHA）

**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）

**Dependencies:** DESC-06, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC

**Deliverables:** Inherited

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_types.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Release write set:** Inherited

**Files likely touched:** `src/command/describe.rs`, `src/command/describe_types.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 需更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md` 与 `COMPATIBILITY.md`（第 219 行原地更新）明确 `--dirty` 不可与 `<COMMIT>` 共用。

**Rollback mode:** immutable-release

**Estimated scope:** M（修改公开 CLI 错误路径）

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self

**Granularity:** `type=implementation; axis=非 --contains 拒绝 --dirty 与 <commit> 共用; recovery=前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=7/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=DESC-06,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-01..10; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-08: 为 `libra describe` 实现 `--broken[=<mark>]`（#803）

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P2；Git CLI 选项兼容

**Issue:** [#803](https://github.com/libra-tools/libra/issues/803)

**Description:** 在 `DescribeArgs` 中增加 `--broken[=<mark>]`（默认 `-broken`）选项（Git `describe.c:742-743` 隐含默认 `--dirty=-dirty`）：可与 `--dirty` 共用；仅文件 stat 变更但内容未变时不追加后缀；真实工作区脏时追加 `--dirty` 标记（默认 `-dirty`）；工作区/index 脏检查失败（broken working tree）时追加 `<mark>`；在非 `--contains` 路径拒绝与 `<commit>` 共用。

**Out of scope:** 无。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DescribeArgs` 有 `--dirty[=<mark>]`，无 `--broken` | `src/command/describe.rs:94-96` |
| `describe_commit_for_atom` 构造 `DescribeArgs` 字面量 | `src/command/describe.rs:159-173` |
| `has_tracked_dirty_changes` 将 status 错误统一转为 `DescribeError::ReadFailure` | `src/command/describe.rs:548-561` |

**Acceptance criteria:**

- [ ] `libra describe --dirty --broken` 在仅文件 stat 变更（如 `touch`）但内容未变时输出不带后缀的描述（覆盖 `#803` / `t6120` 场景）。
- [ ] `libra describe --broken`（不带显式 `--dirty`）在工作区真实脏时追加 `-dirty`。
- [ ] `libra describe --dirty=<dmark> --broken` 在工作区真实脏时追加 `<dmark>`。
- [ ] `libra describe --broken` 在 `has_tracked_dirty_changes()` 失败时追加 `-broken` 而非退出失败。
- [ ] `libra describe --broken=<mark>` 在 `has_tracked_dirty_changes()` 失败时追加自定义 `<mark>`。
- [ ] `libra describe --broken HEAD`（非 `--contains`）返回退出码 129。
- [ ] `libra describe --contains --broken HEAD` 保持被接受（退出码 0）。
- [ ] `libra describe --contains --broken HEAD` 不追加 `-broken` 或 `-dirty` 后缀。
- [ ] ER-06a 强制门（不计入 G-03 上限）：`docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`、`COMPATIBILITY.md`（原地更新第 219 行）同步更新并逐文件验收。

**Verification:**

- [ ] `source .env.test && cargo test --test command_test describe_test::broken_with_dirty_stat_only_change_has_no_suffix` (new)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(describe_test::broken_with_dirty_stat_only_change_has_no_suffix) | test(describe_test::broken_appends_mark_on_corrupt_index) | test(describe_test::broken_implies_dirty_and_rejects_commit)'` (new)
- [ ] `cargo build --bin libra && TC_RUN_ID="desc-08-$(date -u +%Y%m%dT%H%M%SZ)-$$" LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1054-describe-dirty/tc1054-describe-dirty.sh`（TC-1054 / `t6120#109`，同步记录二进制 SHA-256 与 `HEAD` SHA）

**Full-suite trigger:** T-1: 本卡 `patch + 1` 同步 `install.sh` 与 `install.ps1`（ER-13；C 组第④步在 bump 后跑双环境完整 nextest）

**Dependencies:** DESC-07, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC

**Deliverables:** Inherited

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_types.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Release write set:** Inherited

**Files likely touched:** `src/command/describe.rs`, `src/command/describe_types.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 需更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md` 与 `COMPATIBILITY.md`（第 219 行原地更新）补充 `--broken[=<mark>]`。

**Rollback mode:** immutable-release

**Estimated scope:** M（新增 1 处公开 CLI 标志 `--broken`）

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self

**Granularity:** `type=implementation; axis=--broken[=<mark>] 支持与 -dirty/stat-only 回退; recovery=前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=DESC-07,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-01..10; release=independent; split-from=N/A; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `DescribeArgs` 解析（`--no-match` / `--no-long` / `--no-abbrev`）、`compile_globs`、`tag_passes_filters`、`format_describe_result`、`resolve_object_atom_typed`（含常规与命名空间 `<ns>/<tag>-<N>-g<sha>` 回退） | `source .env.test && cargo nextest run --lib -E 'test(command::describe::tests) | test(utils::util::test::resolve_describe_output_suffix) | test(utils::util::test::resolve_describe_noncommit_suffix) | test(utils::util::test::resolve_desc_10_suffix) | test(utils::util::test::resolve_desc_10a_suffix) | test(utils::util::test::resolve_desc_10b_suffix) | test(utils::util::test::resolve_desc_10c_suffix) | test(utils::util::test::resolve_desc_10d_suffix) | test(utils::util::test::resolve_desc_10e_suffix) | test(utils::util::test::resolve_desc_09b_invalid_oid) | test(utils::util::test::resolve_desc_09c_tag_object) | test(utils::util::test::resolve_desc_10f_tag_suffix) | test(utils::util::test::resolve_desc_10g_tag_suffix) | test(utils::util::test::resolve_desc_10h_tag_suffix) | test(utils::util::test::resolve_desc_10i_tag_suffix) | test(utils::util::test::resolve_desc_10j_tag_suffix) | test(utils::util::test::resolve_desc_10k_tag_suffix)'`；逐过滤器分别记录至少一个运行测试，任一为零则失败 |
| 集成 | `--contains ^0`、`--no-match`、`--all --match`、`rev-parse` `<tag>-<N>-g<sha>`、misnamed tag、`--abbrev=0`、`--no-long`、`--no-abbrev --always`、`--dirty <commit>`、`--broken` | `source .env.test && cargo nextest run --test command_test -E 'test(describe_test) | test(describe_long_test) | test(rev_parse_test)'` |
| 兼容 | `compat_version_surface_sync`、`COMPATIBILITY.md:219` 及 `DEP-DESC-TC`（`../libra-testcases/cases/tc{1031,1034,1035,1043,1045,1046,1052,1054}`，上游 `t6120-describe.sh` @ `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`） | `source .env.test && cargo nextest run --test compat_version_surface_sync` 及各卡对应 `TC_RUN_ID="desc-<card>-$(date -u +%Y%m%dT%H%M%SZ)-$$" LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/<tc>/<tc>.sh`（同步记录二进制 SHA-256 与 `HEAD` SHA） |
| 迁移 | N/A（无 SQLite schema 迁移） | N/A |
| 安全 | 损坏 index 仅在 `--broken` 下回退；其余场景与 `--dirty <commit>` 保持 fail-closed | `source .env.test && cargo nextest run --test command_test -E 'test(describe_test::broken)'` |
| 性能 | 不引入额外全仓遍历，沿用现有 BFS/Dijkstra | `source .env.test && cargo nextest run --test command_test -E 'test(describe_test)'` |
| live/gated | N/A（`describe` 为纯本地命令，无远端网络依赖） | N/A |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| DESC-01 | [#795](https://github.com/libra-tools/libra/issues/795) (`src/command/describe.rs:412-496`, `describe_test.rs:687`) | `src/command/describe.rs`, `tests/command/describe_test.rs` | `describe.md` EN/zh-CN, 网站 `describe.en.md`, `COMPATIBILITY.md:219` | `command_test::describe_test::contains_exact_tag_appends_caret_zero` |
| DESC-02 | [#796](https://github.com/libra-tools/libra/issues/796) (`src/command/describe.rs:102-104`) | `src/command/describe.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs` | `describe.md` EN/zh-CN, 网站 `describe.en.md`, `COMPATIBILITY.md:219` | `command_test::describe_test::no_match_resets_previous_match_patterns` |
| DESC-03 | [#797](https://github.com/libra-tools/libra/issues/797) (`src/command/describe.rs:265-289`) | `src/command/describe.rs`, `tests/command/describe_test.rs` | `describe.md` EN/zh-CN, 网站 `describe.en.md`, `COMPATIBILITY.md:219` | `command_test::describe_test::contains_all_respects_match_filter_and_fails_when_unmatched` |
| DESC-09 | [#798](https://github.com/libra-tools/libra/issues/798) / [#799](https://github.com/libra-tools/libra/issues/799) (`src/utils/util.rs:2319-2468`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `rev-parse.md` EN/zh-CN, 网站 `rev-parse.en.md`, `COMPATIBILITY.md:217` | `utils::util::test::resolve_describe_output_suffix`, `command_test::rev_parse_test::test_rev_parse_describe_output_suffix` |
| DESC-09b | #798/#799 后缀错误 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_09b_invalid_oid`, `test_desc_09b_invalid_oid` |
| DESC-09c | #798/#799 tag 对象后缀 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_09c_tag_object`, `test_desc_09c_tag_object` |
| DESC-09a | [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799) 非提交对象 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_describe_noncommit_suffix`, `test_rev_parse_describe_noncommit_suffix` |
| DESC-10a | #798/#799 命名空间回退 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_10a_suffix`, `test_desc_10a_suffix` |
| DESC-10 | #798/#799 命名空间回退 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_10_suffix`, `test_desc_10_suffix` |
| DESC-10b | #798/#799 命名空间回退 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_10b_suffix`, `test_desc_10b_suffix` |
| DESC-10c | #798/#799 命名空间回退 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_10c_suffix`, `test_desc_10c_suffix` |
| DESC-10d | #798/#799 命名空间回退 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_10d_suffix`, `test_desc_10d_suffix` |
| DESC-10e | #798/#799 命名空间回退 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_10e_suffix`, `test_desc_10e_suffix` |
| DESC-10f | #798/#799 tag 对象回退 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_10f_tag_suffix`, `test_desc_10f_tag_suffix` |
| DESC-10g | #798/#799 tag 对象回退 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_10g_tag_suffix`, `test_desc_10g_tag_suffix` |
| DESC-10h | #798/#799 tag 对象回退 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_10h_tag_suffix`, `test_desc_10h_tag_suffix` |
| DESC-10i | #798/#799 tag 对象回退 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_10i_tag_suffix`, `test_desc_10i_tag_suffix` |
| DESC-10j | #798/#799 tag 对象回退 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_10j_tag_suffix`, `test_desc_10j_tag_suffix` |
| DESC-10k | #798/#799 tag 对象回退 | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | rev-parse EN/zh/dev/website, `COMPATIBILITY.md` | `resolve_desc_10k_tag_suffix`, `test_desc_10k_tag_suffix` |
| DESC-05 | [#799](https://github.com/libra-tools/libra/issues/799) (`src/command/describe.rs:225-258`, `describe_format.rs:11-50`) | `src/command/describe.rs`, `src/command/describe_format.rs`, `tests/command/describe_test.rs` | `describe.md` EN/zh-CN, 网站 `describe.en.md`, `COMPATIBILITY.md:219` | `command_test::describe_test::misnamed_annotated_tag_abbrev_zero_forces_full_suffix` |
| DESC-04 | [#798](https://github.com/libra-tools/libra/issues/798) (`src/command/describe.rs:90-92`) | `src/command/describe.rs`, `src/command/describe_tests.rs`, `tests/command/describe_long_test.rs`, `tests/command/describe_test.rs` | `describe.md` EN/zh-CN, 网站 `describe.en.md`, `COMPATIBILITY.md:219` | `command_test::describe_long_test::test_describe_no_long_overrides_long_and_allows_abbrev_zero` |
| DESC-06 | [#800](https://github.com/libra-tools/libra/issues/800) (`src/command/describe.rs:74-80,362-378`) | `src/command/describe.rs`, `src/command/describe_format.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs` | `describe.md` EN/zh-CN, 网站 `describe.en.md`, `COMPATIBILITY.md:219` | `command_test::describe_test::no_abbrev_with_always_outputs_full_sha` |
| DESC-07 | [#802](https://github.com/libra-tools/libra/issues/802) (`src/command/describe.rs:62,94-96`) | `src/command/describe.rs`, `src/command/describe_types.rs`, `tests/command/describe_test.rs` | `describe.md` EN/zh-CN, 网站 `describe.en.md`, `COMPATIBILITY.md:219` | `command_test::describe_test::dirty_with_commit_arg_is_rejected` |
| DESC-08 | [#803](https://github.com/libra-tools/libra/issues/803) (`src/command/describe.rs:94-96,548-561`) | `src/command/describe.rs`, `src/command/describe_types.rs`, `tests/command/describe_test.rs` | `describe.md` EN/zh-CN, 网站 `describe.en.md`, `COMPATIBILITY.md:219` | `command_test::describe_test::broken_with_dirty_stat_only_change_has_no_suffix` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划评审 `PASS`，基线与 `DEP-DESC-DOCS` 核对完成 | 本计划文档进入主线 | `libra revert` 文档提交 |
| M1 | `DESC-01..03`、`DESC-09`、`DESC-09b`、`DESC-09c`、`DESC-09a`、`DESC-10`、`DESC-10a..10k`、`DESC-05`、`DESC-04`、`DESC-06` 全部 `done / complete`（`#541` 已合入且各卡 `DEP-DESC-TC` 输出精确 `TC-<id> PASS` 之后才进入 C/D） | 每卡独立 `patch + 1`、`gh release create`、TC 退出码/二进制 SHA-256 记录与 D 组全绿 | 前滚修复新 patch；已发布可按 `PREV_VER` 降级 |
| M2 | `DESC-07..08` 全部 `done / complete`（含 `DEP-DESC-TC` 对应 TC 通过）且全量收口门全绿 | 每卡独立发布、TC 退出码记录 + 计划级全量 nextest 全绿 | 前滚修复新 patch；已发布可按 `PREV_VER` 降级 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 单卡本地实现或 focused 测试失败 | 未提交工作区改动 | 先核对 `libra status` / `libra diff` 差异归属：仅含本卡改动时执行 `libra restore --staged --worktree -- <card files>` 并清理本卡新增未跟踪文件；混有同文件无关改动时停止并做选择性恢复 | 带着失败测试提交或整文件覆盖无关改动 |
| PR checks 或 `release.yml` 失败 | 已合并提交 | 按 ER-04/ER-10 前滚新建修复卡/新 patch，必要时指引用户降级至 `PREV_VER` | 回退已推送提交或已发布 tag |
| `/Volumes/Sea/libra-backend` 非 `cf` 分支、`origin` 不符、目标 `.en.md` 冲突或含无法隔离的无关超前提交 | 无后端写入 | 保留无关未提交改动（禁止丢弃）；仅在非 `cf`、`origin` 不符、目标文件冲突或无关超前提交无法隔离时停止并报告协调者 | 在非 `cf` 分支写入、丢弃无关改动或混入无关提交 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `libra update-ref` 不支持 `refs/tags/*`（`src/command/update_ref.rs:6-7,147`） | 高：`TC-1045` 阻塞 `DESC-05`，`TC-1043` 阻塞 `DESC-04`，顺序不得颠倒 | `DESC-05` 只等 `#541`+`TC-1045`；`DESC-04` 等 `DESC-05` 完成后再跑 `TC-1043`；未过各自 TC 不进入 C/D | `DESC-05`, `DESC-04` |
| `TC-1046` 可能受前序 `t6120` 状态（`git rev-parse HEAD` rc=129）影响 | 中：非 `describe --no-abbrev` 本身缺陷导致 `TC-1046` 失败 | `DESC-06` ER-02 基线复现时核实是否属前序状态级联，若确认则登记前置阻塞缺陷后再关单 | `DESC-06` |
| `src/utils/util.rs`、`tests/command/rev_parse_test.rs` 与 `rev-parse` 文档和 `branch`/`clone`/`merge-pull`/`global-init-infra`/`plumbing-misc` 写集重叠 | 中：I–I 写集碰撞 | `DESC-09`/`DESC-09b`/`DESC-09c`/`DESC-09a`/`DESC-10`/`DESC-10a..10k` 开工前按 `DEP-DESC-UTIL` 核验上述 5 份计划（含 `PM-05`/`PM-06` 与 `BR-28`、`CL-14`）并刷新行号 | `DESC-09`, `DESC-09a`, `DESC-10`, `DESC-10a..10k` |
| `COMPATIBILITY.md` 增删行导致 `tests/compat-ledger` 行号断言失效 | 中：compat-ledger 门失败 | 所有卡仅原地修改 `COMPATIBILITY.md:217` / `:219` 单行 | `DESC-01..10` |
| 跨仓 `/Volumes/Sea/libra-backend` `cf` 分支含目标文件冲突或无关未发布提交 | 低：阻塞直接推送 | 保留无关未提交改动并仅暂存本卡 `.en.md`；仅在目标文件冲突或无关超前提交无法隔离时停止报告 | `DESC-01..10` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| `describe` 祖先 BFS / `--contains` Dijkstra | $O(V + E)$ 可达提交图 | 无跨命令持久化状态 | 不引入额外全仓对象扫描 | `command_test::describe_test` |
| `--match` / `--exclude` glob 匹配 | $O(R \cdot P)$（$R$ 为 ref 数，$P$ 为模式数，单模式 $\le 256$ B） | 单次命令内 | `MAX_GLOB_LEN = 256` | `command::describe::tests` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已同步（第 219 行 `describe` 原地更新）。
- [ ] `docs/commands/describe.md` 已同步。
- [ ] `docs/commands/zh-CN/describe.md` 已同步。
- [ ] `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md` 已同步。
- [ ] `docs/error-codes.md` 已核对（沿用已有 `LBR-CLI-002`、`LBR-IO-001`、`LBR-REPO-002`）。
- [ ] `tests/INDEX.md` 已核对（复用现有 `command_test` 下的 `describe_test`、`describe_long_test`、`rev_parse_test`，无新增 target）。
- [ ] `Cargo.toml` `[[test]]` 已核对（无需新增 compat target）。
- [ ] `plan-long.md` 日期计划索引已同步或说明 `N/A`。

## Codex review log

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-6（misnamed tag 缺失、`--contains ^0` 仅限附注 tag、`--no-abbrev` 等价 `--abbrev=0`、`--test command_test` target、`--dirty --broken` stat-only 用例、R1 误预填） | 已全部吸收修订 | 只读评审（`2026-11-17 02:57:31 UTC`） |
| R1（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-4（`--no-abbrev` 等价 `--abbrev=0`、`--test command_test` target、`../libra-backend` 核对、`Rollback mode=immutable-release`） | 已全部吸收修订 | 只读评审（`2026-11-17 02:55:17 UTC`） |
| R2（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-4（SQLite tag fixture、`--contains` 忽略 `--dirty`/`--broken`、`--broken` 隐含 `-dirty`、`test_describe_*` 过滤名） | 已全部吸收修订 | 只读评审（`2026-11-17 03:06:05 UTC`） |
| R2（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-2（SQLite tag fixture、`rev-parse` 往返与 `t6120` 场景覆盖） | 已全部吸收修订 | 只读评审（`2026-11-17 03:01:50 UTC`） |
| R3（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-2（新增 `DESC-09` 补齐 `rev-parse` `<tag>-<N>-g<sha>` 及 `DEP-DESC-UTIL`、重计 `DESC-05`/`DESC-08` G-03 谓词） | 已全部吸收修订（含修正 `tag.rs` 锚点、同日计划表、`#795`/`#800` 用例、`DEFER-DESC-04`） | 只读评审（`2026-11-17 03:13:51 UTC`） |
| R3（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-4（`--lib` nextest 门、G-03 谓词计数、可执行降级指引、D 组 job/ref/取证） | 已全部吸收修订（含修正 `prod-files` 排除测试文件） | 只读评审（`2026-11-17 03:10:00 UTC`） |
| R4（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | ERROR | — | 网络 ECONNRESET 中断，未产出 verdict | 只读评审（`2026-11-17 03:29:43 UTC`） |
| R4（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-3（`install.sh -v` 降级参数、D 组具体 CDN URL 与 `gh run view --json jobs`、`DESC-09` 纳入网站文档与 `DEP-DESC-DOCS`） | 已全部吸收修订（含拆分 `DESC-09` AC 失败路径至 7/8） | 只读评审（`2026-11-17 03:21:27 UTC`） |
| R5（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | ERROR | — | 网络 ECONNRESET 中断，未产出 verdict | 只读评审（`2026-11-17 03:38:40 UTC`） |
| R5（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（`DESC-04` `VER=3/8` 不一致） | P2（移除 `gh release download` 备选）已修复 | 只读评审（`2026-11-17 03:33:13 UTC`） |
| R6（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | ERROR | — | 网络 ECONNRESET 中断，未产出 verdict | 只读评审（`2026-11-17 03:42:07 UTC`） |
| R6（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-2（未提交改动用 `libra restore` 而非 `libra revert`、后端直接 `cf` 不擅自独立克隆） | 已全部吸收修订 | 只读评审（`2026-11-17 03:41:42 UTC`） |
| R7（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | ERROR | — | 网络 ECONNRESET 中断，未产出 verdict | 只读评审（`2026-11-17 03:53:10 UTC`） |
| R7（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（整文件 `libra restore` 可能覆盖同文件无关改动） | 已增加 `libra diff` 差异归属核对门与选择性恢复停止条件 | 只读评审（`2026-11-17 03:45:31 UTC`） |
| R8（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-3（`utils::util::test` 模块名、`DESC-05`/`08` G-03 严格重计、misnamed tag 限定非 `--contains` 路径） | 已全部吸收修订（含 `rev_parse_test` 命名用例与 `COMPATIBILITY.md:217`） | 只读评审（`2026-11-17 04:08:15 UTC`） |
| R8（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | PASS | 无 P0/P1 | 无 | 只读评审（`2026-11-17 03:56:02 UTC`） |
| R9（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | PASS | 无 P0/P1 | P2 4 项（`DESC-09` 短哈希/语法细节、`128 vs 129` 退出码记录、`DESC-01` 文案）记录备查 | 只读评审（`2026-11-17 04:13:02 UTC`） |
| R9（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（缺少上游 `t6120-describe.sh` / TC 验收门与不可用回落） | 已新增 `DEP-DESC-TC` 外部门与 fail-closed 判据 | 只读评审（`2026-11-17 04:12:32 UTC`） |
| R10（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-3（`DEP-DESC-TC` 未接入卡片依赖/命令、未锚定 `../libra-testcases/cases/tc*` 与完整 SHA、未绑定 `blocked` 状态） | 已全部吸收修订 | 只读评审（`2026-11-17 04:15:21 UTC`） |
| R10（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（`DEP-DESC-TC` 未接入卡片依赖与可执行验证命令） | 已全部吸收修订 | 只读评审（`2026-11-17 04:14:46 UTC`） |
| R11（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | ERROR | — | 网络 ECONNRESET 中断，未产出 verdict | 只读评审（`2026-11-17 04:21:16 UTC`） |
| R11（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（`DESC-05` JSON 合并谓词展开超 8/8） | 已拆分为 7/8 单一谓词 | 只读评审（`2026-11-17 04:22:17 UTC`） |
| R12（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | CANCELLED | — | 超过 7 分钟无输出后取消，合并至 R13 复审 | 只读评审（`2026-11-17 04:32:23 UTC`） |
| R12（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（`DESC-05` `--contains` 条目仍含双谓词，实际为 `8/8`） | 已拆分为 2 项独立谓词并更新 `AC=8/8` | 只读评审（`2026-11-17 04:27:46 UTC`） |
| R13（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | FAIL | P1-1（`DESC-01/03/06/07/09` 仍有双谓词 AC） | 已全部拆分并重计 AC，顺带吸收 2 项 P2 | 只读评审（`2026-11-17 04:36:06 UTC`） |
| R13（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-2（`DESC-06` 8/8 与 `DESC-08` 8/8 双谓词未拆） | 已全部拆分并同步审计表与 Granularity | 只读评审（`2026-11-17 04:36:08 UTC`） |
| R14（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | PASS | 无 P0/P1；R13 整改全部确认 | P2（`DESC-01` axis 文案、历史表 R4 笔误、129 约定）记录在案 | 只读评审（`2026-11-17 04:42:25 UTC`） |
| R14（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（`DESC-03` AC1、`DESC-07` AC1-2、`DESC-08` AC6 仍合并退出码与错误类型） | 已改为单一退出码/错误类型谓词 | 只读评审（`2026-11-17 04:39:33 UTC`） |
| R15（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | CANCELLED | — | Codex R15 已指出 `DEP-DESC-TC` 门禁问题，取消后合并至 R16 复审 | 只读评审（`2026-11-17 04:48:00 UTC`） |
| R15（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（`DEP-DESC-TC` 与完成判据允许整脚本非 0 时靠 `DEFER-*` 关单） | 已收紧为必须完整 `tc*.sh` 退出码 0 方可 `done / complete` 并关闭 Issue | 只读评审（`2026-11-17 04:45:36 UTC`） |
| R16（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | FAIL | P1-1（`DESC-04`/`05` 的 `TC-1043`/`1045` 依赖未注册的 `#541` `update-ref refs/tags/*`） | 已注册 `DEP-DESC-541` 并接入 `DESC-04`/`05`；要求 `TC-<id> PASS` 精确输出行与记录 `TC-1046` 风险 | 只读评审（`2026-11-17 04:51:12 UTC`） |
| R16（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（`tc*.sh` 仅按映射子用例返回 0/非 0，而非整份 `t6120` 进程退出码） | 已按 `tc-harness.sh:486,580` 澄清单 TC 门禁并要求同时记录上游 `exit` 文件 | 只读评审（`2026-11-17 04:50:48 UTC`） |
| R17（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | ERROR | — | 网络 ECONNRESET 中断，未产出 verdict | 只读评审（`2026-11-17 04:58:59 UTC`） |
| R17（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（`DESC-09` 未明确 40/64 位全长 `<sha>` 的对象存在性与提交类型校验） | 已在 `DESC-09` 补充全长/缩写存在性与非提交对象校验（`AC=8/8`） | 只读评审（`2026-11-17 04:56:16 UTC`） |
| R18（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | CANCELLED | — | Codex R18 已指出 `DEP-DESC-DOCS`、`DESC-01` `exact_match` 与 `DESC-09` 命名空间前缀问题，取消后合并至 R19 复审 | 只读评审（`2026-11-17 05:02:00 UTC`） |
| R18（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-3（`DEP-DESC-DOCS` 路径硬编码、`DESC-01` `data.exact_match`、`DESC-09` `tags/`/`heads/`/`remotes/` 命名空间回退） | 已全部吸收修订并同步审计表 | 只读评审（`2026-11-17 05:00:53 UTC`） |
| R19（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | CANCELLED | — | Codex R19 已指出 `DESC-09` 超限问题，取消后合并至 R20 复审 | 只读评审（`2026-11-17 05:06:00 UTC`） |
| R19（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（`DESC-09` 含 3 种命名空间回退超 G-03 上限） | 已拆分为 `DESC-09`（`7/8`）与 `DESC-10`（`5/8`）并同步依赖与审计表 | 只读评审（`2026-11-17 05:04:46 UTC`） |
| R20（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | CANCELLED | — | Codex R20 已指出 `DEP-DESC-DOCS`/`DESC-10` 追溯表问题，取消后合并至 R21 复审 | 只读评审（`2026-11-17 05:10:00 UTC`） |
| R20（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-2（`DEP-DESC-DOCS` remote 核验与可执行 `typecheck`/`build` 命令、`DESC-10` 漏入追溯表/M1/Phase 1） | 已全部吸收修订 | 只读评审（`2026-11-17 05:09:20 UTC`） |
| R21（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | CANCELLED | — | Codex R21 已指出 `M1`/`DESC-09` G-09/故障恢复矩阵问题，取消后合并至 R22 复审 | 只读评审（`2026-11-17 05:13:00 UTC`） |
| R21（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-3（`M1` 漏 `DESC-10`、`DESC-09` 漏 G-09 拆出声明、故障恢复矩阵脏工作区停止冲突） | 已全部吸收修订（含同步风险登记） | 只读评审（`2026-11-17 05:12:20 UTC`） |
| R22（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | CANCELLED | — | Codex R22 已指出 Phase 1 退出条件漏 `DESC-10`，取消后合并至 R23 复审 | 只读评审（`2026-11-17 05:16:00 UTC`） |
| R22（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（Phase 1 退出条件漏 `DESC-10`） | 已补齐 Phase 1 退出条件、适用范围与关联表中的 `DESC-10` | 只读评审（`2026-11-17 05:15:29 UTC`） |
| R23（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | CANCELLED | — | Codex R23 已指出 `DEP-DESC-UTIL` 与测试矩阵漏 `DESC-10`，取消后合并至 R24 复审 | 只读评审（`2026-11-17 05:19:00 UTC`） |
| R23（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-2（`DEP-DESC-UTIL` 未门控 `DESC-10`、测试矩阵漏 `resolve_namespaced_describe_output_suffix`） | 已全部吸收修订 | 只读评审（`2026-11-17 05:18:40 UTC`） |
| R25（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | CANCELLED | — | Codex R25b 已指出 `../libra-backend` 与 `/Volumes/Sea/libra-backend` 路径确认门，取消后合并至 R26 复审 | 只读评审（`2026-11-17 05:28:00 UTC`） |
| R25（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（`../libra-backend` 与 `AGENTS.md` `/Volumes/Sea/libra-backend` 路径未显式要求开工前确认） | 已在 `DEP-DESC-DOCS` 增加 `/Volumes/Sea/libra-backend` 缺失时向协调者确认门，同时保留 `plan-template.md:650` `../libra-backend` 写法 | 只读评审（`2026-11-17 05:27:05 UTC`） |
| R26（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | CANCELLED | — | Codex R26 已指出网站文档缺少 Claude Code `PASS` 门，取消后合并至 R27 复审 | 只读评审（`2026-11-17 05:33:00 UTC`） |
| R26（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（`DEP-DESC-DOCS` 缺少网站文档变更 Claude Code `PASS` 前置门） | 已补齐 Claude Code `PASS` 门作为 `cf` 提交/推送/完成前置 | 只读评审（`2026-11-17 05:31:52 UTC`） |
| R27（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-3（`DEP-DESC-UTIL` 锚点错误且漏 `clone`/`merge-pull`/`global-init-infra` 与同日表、`DEP-DESC-541` 阻塞串行链、`DESC-01`/`09` G-03 谓词超 8） | 已全部吸收修订（含 5 项 P2：`AGENTS.md:135` 归属、`.libra` 单一 VCS 与 `--write`、`DEFER-DESC-02`→`GII-02`、`Phase 0` `plan-status.md` 登记门、`DESC-10` 追溯表函数名） | 只读评审（`2026-11-17 06:51:25 UTC`） |
| R27（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1..P1-3（`DEP-DESC-DOCS` `--only describe rev-parse` 跨页漂移、`DEP-DESC-TC` 缺唯一 `TC_RUN_ID` 与二进制校验、D 组 `jobs[].name` 与 4-leg matrix/event/ref） | 已全部吸收修订 | 只读评审（`2026-11-17 06:49:33 UTC`） |
| R28（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | FAIL | 网络中断（`ECONNRESET`），合并至 R29 重审 | — | 只读评审（`2026-11-17 07:42:48 UTC`） |
| R28（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1：`DEP-DESC-541` 允许未过 TC 即 `Acceptance=complete`，与 `DEP-DESC-TC`/ER-04 A 组矛盾 | 已改为 TC `PASS` 后才进入 C/D | 只读评审（`2026-11-17 07:42:48 UTC`） |
| R29（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | FAIL | 网络中断（`ECONNRESET`），合并至 R30 | — | 只读评审（`2026-11-17 08:05:51 UTC`） |
| R29（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1：Phase 1 退出条件未要求 `DESC-06` `done / complete`，与 Phase 目标及 M1 不一致 | 已补入退出条件 | 只读评审（`2026-11-17 08:05:51 UTC`） |
| R30（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | FAIL | 网络中断（`ECONNRESET`），合并至 R32 | — | 只读评审（`2026-11-17 08:21:00 UTC`） |
| R31（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1：`COMPATIBILITY.md` 缺实现阶段互斥门 | 已加 `DEP-DESC-COMPAT` 并接入各卡 | 只读评审（`2026-11-17 08:25:00 UTC`） |
| R32（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | Pending | — | — | 待执行只读复审 |
| R32（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | Pending | — | — | 待执行只读复审 |
| R35（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | 无新增 P1；仅完成局部检查，不能作为完整复审 | R36 完整复审，不以局部检查推定其它维度通过 | 只读评审（部分覆盖） |
| R36（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1: Phase 0 仍双审、网站缺独立 Claude Code PASS；P2: 文头修订时间陈旧。已改 Phase 0 为 Codex 单审、恢复网站独立 Claude Code PASS 并同步时间 | R37 完整复审 | Codex R36 只读评审（完整覆盖） |
| R37（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | 无新增 P1；未完成全部卡片与源码锚点复审，不能作为完整结论。已统一现行网站路径并保留历史记录 | R38 完整复审 | Codex R37 只读评审（部分覆盖） |
| R38-A（Codex `gpt-6.1-terra`） | 第 1–598 行，部分核对 | FAIL | P1：DESC-09 错误拒绝非提交对象；Git 可解析指向 blob 的表达式。已改为 blob/tree/commit/tag 与 Git 对照；本次未完成该区间全部合同核对 | R39-A 完整分区复审 | Codex R38-A 只读评审（部分覆盖） |
| R39-A（Codex `gpt-6.1-terra`） | 第 1–598 行及跨界依赖 | FAIL | P1：DESC-10 仍限制提交类型且漏完整 refs 命名空间。已覆盖六种前缀及 blob/tree/commit/tag、全长/缩写与字面 ref 优先回归 | R40-A 完整分区复审 | Codex R39-A 只读评审（分区覆盖） |
| R40-A（Codex `gpt-6.1-terra`） | 第 1–600 行及相关跨界卡（部分验证） | FAIL | P1：DESC-09/10 合并多个独立判据、实际超过 G-03 上限；非法后缀与字面引用优先冲突。已限定语法检查仅在字面引用未命中时执行，拆分与全区复审待完成 | 已拆分 `DESC-09a`、`DESC-10a`；重新完整分区复审 | Codex R40-A 只读评审（未完成全部合同核验） |
| R41-A（Codex `gpt-6.1-terra`） | 第 1–650 行及 DESC-09a/10a 完整卡 | FAIL | P1：DESC-09 字面引用优先及 DESC-10/10a 不同对象长度组合仍合并计数；新卡文档字段未逐文件列举。已补文档字段，计数与发布切片重拆待完成 | 完成拆分后复审 | Codex R41-A 只读评审（分区覆盖） |
| R47-A（Codex `gpt-6.1-terra`） | DESC-09 系列及 DESC-10/10a 定向检查 | FAIL | P1：集中单测矩阵漏新增测试；P2：顺序摘要漏 DESC-09c；其余指定范围因输出截断未完成 | 已补矩阵和顺序，须完整复审 | Codex R47-A 只读部分审查 |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-DESC-01 | 嵌套 tag（tag-of-tag）多层剥皮（`#498`） | 已有 `DEFER-TT-08` 跟踪，不在本批 8 个 Issue 范围 | `#498` 重启时 | `COMPATIBILITY.md` (`#498 DEFER-TT-08`) |
| DEFER-DESC-02 | `libra describe` 配合全局 `--git-dir`/`--work-tree`（`#801`） | 需要全局仓库发现选项改造，超出 `describe` 单命令范围 | 已有计划排期实现 | `plan-20261117-global-init-infra.md`（`GII-02`） |
| DEFER-DESC-03 | `libra describe --contains --all` 去除分支 `heads/` 前缀 | 保持 Libra 现有 `--all` 统一前缀契约，避免破坏既有 `--all` 断言 | 需统一调整 `--all` 命名空间输出时 | `COMPATIBILITY.md` (`describe` 行) |
| DEFER-DESC-04 | `libra describe --contains --exact-match`（既有差异：Git 在 `--contains` 下忽略 `--exact-match`） | 既有设计选择（`describe.rs:390-393`），不在 `#795` 变更范围 | 需与 Git `--contains --exact-match` 完全对齐时 | `COMPATIBILITY.md` (`describe` 行) |

## 完成判据

- [ ] 所有任务卡满足粒度规则 `G-*`，「任务卡粒度审计表」已填齐。
- [ ] `DESC-01..10`、`DESC-09a`、`DESC-09b`、`DESC-09c`、`DESC-10a..10k` 全部 `Lifecycle=done` 且 `Acceptance=complete`（ER-04）。
- [ ] 所有任务的 Verification 命令（含 `DEP-DESC-TC` 对应 `../libra-testcases` 脚本）已运行并在本计划内记录唯一 `TC_RUN_ID`、二进制 SHA-256、`HEAD` SHA、`tc*.sh` 退出码（必须 `0`）、精确 `TC-<id> PASS` 输出行与 `TC_RUN_DIR` 上游 `exit` 文件值；对应 `tc*.sh` 通过后方才进入该卡 C/D 并标 `done / complete`、关闭 Issue。`DESC-05` 在 `#541` 未合入或 `TC-1045` 未 `PASS` 前保持 `blocked`；`DESC-04` 在 `DESC-05` 未完成或 `TC-1043` 未 `PASS` 前保持 `blocked`。
- [ ] **计划完成门 / 全量收口门（ER-13）**：在全部任务卡完成后运行一次 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿。
- [ ] 全量收口门暴露的 Bug 已全部前滚修复并重跑全量至全绿。
- [ ] 必要的 docs/compat/error-code/test-index 更新已完成。
- [ ] 每张卡的 `Rollback mode` 均已核验。
- [ ] Codex review 最终结论为 `PASS`，P0/P1 全部关闭（按用户 2026-11-17 指令，不再要求 Claude Review）。
- [ ] 每张会发布卡已完成 `patch + 1`、版本面同步（`compat_version_surface_sync`）、工具链刷新 `Cargo.lock`、`gh release create` 与 D 组证据取证。
- [ ] 「修订历史」已记录全部规范性变更。
- [ ] 未新建证据文件或证据文件夹（v2.12）。
- [ ] `plan-long.md` 与 `plan-status.md` 已同步。
