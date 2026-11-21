# `describe` 命令 Git 兼容性修复计划（2026-11-17）

> 模板版本：`v2.16`；继承 [plan-template.md](plan-template.md) 的 `GC-01..13`、`ER-01..14`、`G-01..11`。
> 状态：Claude R65 与 Codex R65 双审 PASS（受审 SHA-256 前 12 位 `10e7257d1dda`；R65 为差异确认轮，其基线 R64 字节经 Codex 四分区全文 PASS、Claude 全文唯一 P1 已于 R65 修正），计划可提交；全部任务 `pending / 空`，尚未执行、尚未修改代码、尚未发布版本。计划文本提交门：按用户 2026-11-18 指令，须由 Claude（Subagent）与 Codex 对同一字节双审均 `VERDICT: PASS` 方可提交（取代 2026-11-17 的 Codex 单审口径）；执行期各卡 ER-05 评审与网站交付的独立 Claude Code PASS 另按各自门执行。
> 源码基线：`62abb622f0eb2626934633ada78061963ebf1f44`（2026-11-18 在 `49b62d1a40d3e22f7a778bda9678280e7292826a` 上复核锚点，`src/cli.rs` 分发行更新为 `:4004`）；创建时间：`2026-11-17 02:52:00 UTC`；最近修订：`2026-11-18 17:13:51 UTC`。
> 覆盖 Open Issues：[#795](https://github.com/libra-tools/libra/issues/795)、[#796](https://github.com/libra-tools/libra/issues/796)、[#797](https://github.com/libra-tools/libra/issues/797)、[#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)、[#800](https://github.com/libra-tools/libra/issues/800)、[#802](https://github.com/libra-tools/libra/issues/802)、[#803](https://github.com/libra-tools/libra/issues/803)（共 8 个；另含 `DESC-09` 补齐 `rev-parse` 对 `<tag>-<N>-g<sha>` 的往返解析，以闭环 `#798`/`#799` 验收）。

## 文档职责

- **网站交付路径（已确认）:** 唯一生产网站 checkout 为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支，即本仓库同级 `../libra-backend`，与模板 ER-06a 默认相邻工作区一致）；依据：用户 2026-11-18 11:55:09 UTC 确认。`AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`（本机不存在），属已知文档滞后，本计划不修改 `AGENTS.md`；`AGENTS.md:135` 的交付流程（直接在 `cf` 上工作、保留无关改动、不建分支/PR/worktree、同步检查与前端验证、Claude Code `PASS`、提交推送并核验 Cloudflare 部署）仍适用。2026-11-18 核对：该路径存在、仅含 `.libra`、`libra status --short --branch` 为 `## cf...origin/cf`；写入前按「文档与网站验收命令」第 2 步重新核验。

本文解决 `libra describe` 在 Git 兼容性测试（`t6120-describe.sh`）中暴露的 8 项选项解析、输出格式、过滤语义与错误路径缺陷，目标是逐卡交付可独立验收与发布的修复切片，使 `libra describe` 在 `--contains`、`--no-match`、`--all` + `--match`、misnamed annotated tag（`--abbrev=0`）、`--no-long`、`--no-abbrev`、`--dirty <commit>`、`--broken` 及 `rev-parse` 往返解析场景下与 Git 行为一致，并同步 `--json` 结构化输出与双语/网站文档。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收。

### 适用范围

- 命令与模块：`libra describe`（`src/command/describe.rs`、`src/command/describe_format.rs`、`src/command/describe_types.rs`、`src/command/describe_tests.rs`）及共享修订解析器（`src/utils/util.rs` 仅 `DESC-09`/`DESC-09b`/`DESC-09c`/`DESC-09a`/`DESC-10`/`DESC-10a..10k`）。
- 机器输出与错误码：`--json` 输出结构 `DescribeOutput`、`DescribeError` 与 `StableErrorCode`（`LBR-CLI-002`、`LBR-IO-001` 等）。
- 测试与文档：`tests/command/describe_test.rs`、`tests/command/describe_long_test.rs`、`tests/command/rev_parse_test.rs`（均归属 `--test command_test`）、`docs/commands/{describe,rev-parse}.md`、`docs/commands/zh-CN/{describe,rev-parse}.md`、`docs/development/commands/{describe,rev-parse}.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/{describe,rev-parse}.en.md`（及网站构建生成产物，不提交）、`COMPATIBILITY.md`（仅原地更新第 217、219 行，不增删行）。

### 非目标

- 不改变 `for-each-ref` 的 `%(describe)` atom 接口签名（仅保持 `describe_commit_for_atom` 兼容，新增 `DescribeArgs` 字段在 `describe.rs:159-173` 填默认值）。
- 不实现嵌套 tag（tag-of-tag）多层剥皮（由 `#498 DEFER-TT-08` 跟踪）。
- 不实现全局 `--git-dir`/`--work-tree`（`#801` 登记为 `DEFER-DESC-02`，已由 `plan-20261117-global-init-infra.md` 的 `GII-02`、`GII-24` 承接）。
- 不修改 `libra update-ref` 以支持 `refs/tags/*`（`src/command/update_ref.rs:6-7, 147`）；misnamed tag 测试夹具通过 `libra tag Q <tag-oid>`（`src/command/tag.rs:817-869` 目标类型检测保留未剥皮的 tag 对象）+ `libra tag -d A` 在 SQLite `reference` 表构造。
- 不改变 `--contains --all` 展示分支前缀 `heads/` 的现有格式（`DEFER-DESC-03`），也不改变既有的 `--contains --exact-match` 差异（`DEFER-DESC-04`）。
- 不新增 SQLite schema migration、不修改 `src/cli.rs` 子命令注册。

### 成功定义

- 上述 8 个 Open Issues 全部映射到具体任务卡并逐卡通过本地验收（`--test command_test` focused 用例、`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、nextest 发布门）、外部上游 `t6120-describe.sh`/TC 门（`DEP-DESC-TC`）、ER-05 review `PASS` 与 C/D 发布门。
- `--json` 与人类可读输出在所有新增/修复选项下均保持字段一致与可解析性。
- 中英文命令文档、`COMPATIBILITY.md`（原地更新第 217、219 行）与 `/Volumes/Data/gitmono/libra-backend` `cf` 分支网站文档随对应任务卡同卡同步。
- 全部任务卡达到 `done / complete` 且全量收口门通过后，本计划方可标记完成。

## 使用规则

- 日期计划正文、`plan-status.md` 状态推进与「修订历史」每次写入均记录 `YYYY-MM-DD HH:MM:SS UTC` 时间戳。
- 任何执行本计划的 Agent 在推进或完成一张任务卡时，必须在同一变更中同步更新 [plan-status.md](plan-status.md)（Phase 0 开工前须先在 `plan-status.md` 登记 `plan-20261117-describe.md` 状态行）。
- 禁止在 `docs/development/plan/` 或仓库其它位置新建 `evidence/`、`baseline/`、`scratch/` 等证据目录或文件；所有验收摘要、测试计数与门结果均写入本计划正文。
- 每个任务卡开工前必须执行 ER-01（`libra status --short --branch`）与 ER-02（刷新源码行号与当前行为复现）。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。以下基线核对自 `62abb622f0eb2626934633ada78061963ebf1f44`（`2026-11-17 02:50:04 UTC`），2026-11-18 在 `49b62d1a40d3e22f7a778bda9678280e7292826a` 上复核。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `DescribeArgs`、`describe_commit_for_atom`、`run_describe`、`run_describe_contains`、`apply_dirty_mark`、`describe_cli_error` | `src/command/describe.rs:60-115, 152-186, 188-381, 394-531, 533-561, 627-669` |
| 格式化逻辑 | `describe_output`、`abbreviate_hash`、`format_describe_result` | `src/command/describe_format.rs:3-50` |
| 类型与引用 | `DescribeOutput`、`DescribeError`；`TagObject::Tag(tg)` 的 `tg.tag_name` 承载内部标签名；`tag::list` 从 SQLite 读取 tag 引用；`tag.rs` 目标类型检测保留未剥皮 tag 对象 | `src/command/describe_types.rs:5-54`, `src/internal/tag.rs:370-410`, `src/command/tag.rs:817-869` |
| 修订解析 | `resolve_object_atom_typed` 仅尝试 ref 与纯十六进制前缀，不支持 `<tag>-<N>-g<sha>` | `src/utils/util.rs:2319-2335, 2404-2468` |
| CLI 注册 | `Commands::Describe(command::describe::DescribeArgs)` 路由到 `execute_safe` | `src/cli.rs:539, 4004` |
| 单元测试 | `describe_tests.rs` 覆盖错误文案与 `compile_globs`/`tag_passes_filters` | `src/command/describe_tests.rs:1-92` |
| 集成测试 | `describe_test`（850 行，含 `:687` 现有 `--contains --exact-match` 断言）、`describe_long_test`；均挂载在 `--test command_test` | `tests/command/mod.rs:1039-1040`, `tests/command/describe_test.rs:90-135, 687`, `tests/command/describe_long_test.rs:4-75` |
| 命令文档 | EN/zh-CN 命令页与相邻仓库 `/Volumes/Data/gitmono/libra-backend` 网站 EN 文档存在 | `docs/commands/describe.md:1-100`, `docs/commands/zh-CN/describe.md:1-100`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md` |
| 兼容矩阵 | `describe` 标为 `partial`（`tests/compat-ledger` 固定行号，必须原地更新） | `COMPATIBILITY.md:219` |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-DESC-01 | `run_describe_contains` 对附注 tag 精确匹配（tag 种子状态，含主循环与 `--exact-match`/`--candidates 0` 提前返回分支）未追加 `^0`，且在 `--contains` 中错误调用 `apply_dirty_mark` | 与 `git describe --contains` 不一致（#795） | `src/command/describe.rs:417, 427, 465-469, 496`, `tests/command/describe_test.rs:555, 687` | `DESC-01`、`DESC-01a`（`^0`）、`DESC-07`（`--contains` 忽略 `--dirty`） |
| GAP-DESC-02 | `DescribeArgs` 缺少 `--no-match`，无法重置先前的 `--match` 过滤模式 | 无法覆盖/清除 `--match`（#796） | `src/command/describe.rs:102-104` | `DESC-02` |
| GAP-DESC-03 | `--all` 加载 `heads/*` 与 `remotes/*` 时未应用 `--match`/`--exclude` | `--contains --all --match` 忽略 `--match` 并误匹配未过滤分支（#797） | `src/command/describe.rs:265-289` | `DESC-03` |
| GAP-DESC-04 | `resolve_object_atom_typed` 不支持 `<tag>-<N>-g<sha>` 语法 | `#798`/`#799` 的 `rev-parse` 往返验收无法解析 describe 输出 | `src/utils/util.rs:2319-2335, 2404-2468` | `DESC-09`、`DESC-09a..09c`、`DESC-11`、`DESC-10`、`DESC-10a..10k`（语义见 `ADR-DESC-03`） |
| GAP-DESC-05 | `tag_map` 未使用 `TagObject::Tag(tg).tag_name`，且无 `misnamed` 标志在 `--abbrev=0` / `--no-long` 时强制输出 `-N-g<sha>` | misnamed annotated tag（`refs/tags/Q` → 内部名 `A`）在 `--abbrev=0`（#799）与 `--no-long`（#798）下输出错误 | `src/command/describe.rs:225-258`, `src/command/describe_format.rs:11-50` | `DESC-05` |
| GAP-DESC-06 | `DescribeArgs` 缺少 `--no-long`，无法取消先前的 `--long` 标志 | `--long --no-long` 被 clap 拒绝（#798） | `src/command/describe.rs:90-92, 195-198` | `DESC-04` |
| GAP-DESC-07 | `DescribeArgs` 缺少 `--no-abbrev`（Git 中等价于 `--abbrev=0`） | `--no-abbrev` 被 clap 拒绝（#800） | `src/command/describe.rs:74-80, 194-198, 362-378` | `DESC-06` |
| GAP-DESC-08 | `run_describe` 未在非 `--contains` 路径拒绝同时传入 `--dirty` 与 `<commit>` 参数 | `libra describe --dirty <commit>` 静默执行工作区脏检查（#802） | `src/command/describe.rs:62, 94-96, 188-190` | `DESC-07` |
| GAP-DESC-09 | `DescribeArgs` 缺少 `--broken[=<mark>]` 标志 | `libra describe --broken` 被 clap 拒绝（#803） | `src/command/describe.rs:94-96, 533-561` | `DESC-08` |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-long.md` | Git 命令兼容性长期能力 | 完成后按需更新日期计划索引，不复制长期路线图 |
| 同批 `plan-20261117-*.md` 中写 `src/utils/util.rs`、`tests/command/rev_parse_test.rs` 或 `rev-parse` 文档（EN/zh-CN/开发/网站）的卡：`plan-20261117-branch.md`（`BR-06`、`BR-07`、`BR-25`..`BR-32`、`BR-40`；`BR-28` 仅 `util.rs`）、`plan-20261117-clone.md`（`CL-14`、`CL-23`、`CL-28`）、`plan-20261117-merge-pull.md`（`MP-30`）、`plan-20261117-global-init-infra.md`（`GII-02`、`GII-03`、`GII-04`、`GII-12`、`GII-23`；其中 `GII-12` 写 `rev_parse_test.rs` 与 `rev-parse` 文档，`GII-23` 亦写 `checkout` 文档）、`plan-20261117-plumbing-misc.md`（`PM-05`、`PM-06`、`PM-18`） | 与 `DESC-09`、`DESC-09a..09c`、`DESC-11`、`DESC-10`、`DESC-10a..10k` 的写集相交（2026-11-18 核对各计划当前文本；以开工当日各计划写集为准） | 经 `DEP-DESC-UTIL` 串行：每张相关卡开工前核对上述计划无在跑卡持有这些文件，并刷新 `util.rs` 行号；只按计划名与卡 ID 引用，不修改其它计划 |
| 同批全部 `plan-20261117-*.md`（`branch`、`checkout-switch-restore`、`clone`、`commit-signing`、`diff`、`fetch-push`、`format-patch`、`global-init-infra`、`grep`、`log-show`、`merge-pull`、`plumbing-misc`、`sequencer-am`、`worktree-clean-reset-apply`） | 共享 `COMPATIBILITY.md`；所有网站交付共享 `/Volumes/Data/gitmono/libra-backend` 的生成产物（`docs-manifest.ts`、`docs-search.ts`、`dist/`）；`global-init-infra` 以 `DEP-GII-03` 登记同批串行窗口 | `COMPATIBILITY.md` 只原地修改 `:217`/`:219` 并经 `DEP-DESC-COMPAT` 串行；网站交付经 `DEP-DESC-DOCS` 取得协调者串行窗口（与 `DEP-GII-03` 同一协调者窗口）；发布遵循 ER-12 单一发布者 |
| `AGENTS.md:135` | 网站交付流程说明仍写 `/Volumes/Sea/libra-backend`（本机不存在） | 已知文档滞后：本批计划按用户 2026-11-18 11:55:09 UTC 确认使用 `/Volumes/Data/gitmono/libra-backend`，本计划不修改 `AGENTS.md`；其交付流程（直接在 `cf` 上工作等）仍适用 |
| `docs/development/plan/issues/541.md` | `#541`：`update-ref` 支持 `refs/tags/*`，是上游 `t6120` 改名场景前置 | 经 `DEP-DESC-541` 门控 `DESC-05`/`DESC-04` 的 TC 与发布 |
| `docs/commands/describe.md` / `zh-CN/describe.md` / `rev-parse.md` / `COMPATIBILITY.md` | 命令用户文档与兼容矩阵 | 每张涉及公开 CLI 行为的卡按 ER-06a 同卡同步；`COMPATIBILITY.md` 只原地更新 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 8 个 Open Issues 均属 `describe` Git 兼容缺口，补入 `DESC-09` 实现 `rev-parse` `<tag>-<N>-g<sha>` 往返以完整闭环 `#798`/`#799` | 按 `DESC-01 -> DESC-01a -> DESC-02 -> DESC-03 -> DESC-09 -> DESC-09b -> DESC-09c -> DESC-09a -> DESC-10 -> DESC-10a -> DESC-10b -> DESC-10c -> DESC-10d -> DESC-10e -> DESC-10f -> DESC-10g -> DESC-10h -> DESC-10i -> DESC-10j -> DESC-10k -> DESC-11 -> DESC-05 -> DESC-04 -> DESC-06 -> DESC-07 -> DESC-08 -> DESC-08a` 排定 |
| 可行性 | `DESC-01..08`、`DESC-01a`、`DESC-08a` 集中在 `src/command/describe*.rs`；`DESC-09`、`DESC-09a..09c`、`DESC-10..10k` 改动 `src/utils/util.rs`，作为家族子卡由 `DESC-11` 按 `T-2` 承担全量门 | 均在 M 规模内；describe 卡独立交付，`rev-parse` 卡经家族发布点统一交付 |
| 任务卡粒度 | 每卡 1 个行为轴；实现卡 AC/VER 逐独立谓词精确计数且均 ≤ 8、landing=1、scope=M；家族发布点 `DESC-11` 为 `release` 卡（AC/VER ≤ 12、无行为落点） | 满足 `G-01..11`，无需 waiver |
| 依赖与顺序 | `DESC-01..08`、`DESC-01a`、`DESC-08a` 共享 `src/command/describe.rs`；`DESC-09`、`DESC-09a..09c`、`DESC-10`、`DESC-10a..10k` 共享 `src/utils/util.rs`，并与同批 5 份计划相交；根命名空间四卡与前缀命名空间十二卡以家族 `REL-DESC-09` 经 `DESC-11` 统一发布（见关系表） | 全串行并登记 `DEP-DESC-UTIL`、`DEP-DESC-COMPAT` 与 `DEP-DESC-DOCS` |
| 完整性 | 包含单元测试、nextest 集成门（含 `t6120` 场景与 `rev-parse` 往返）、EN/zh-CN/网站文档、`COMPATIBILITY.md:217/219`、可执行 `immutable-release` 降级指引与完整 C/D 门 | 全覆盖 ER-04/06a/08/13/14 |
| 安全性 | 不引入外部网络、不接触全局配置库、遵循 `LBR-CLI-002` fail-closed | 继承 GC-07/GC-11 |
| 功能正确性 | 已对照 `builtin/describe.c`、`t6120-describe.sh` 与 pinned Git（`2.56.0.116.g6de20f6092`）`object-name.c:1028-1141` 校准（describe 后缀语义见 `ADR-DESC-03`） `--contains` 忽略 `--dirty`/`--broken`、`--contains ^0` 仅限附注 tag、misnamed tag、`--no-abbrev == --abbrev=0`、`--broken` 隐含 `-dirty` | 逐卡附正向与负向用例 |
| 接口兼容 | 保持 `DescribeOutput` JSON 向后兼容（不删除或重命名已有字段） | 在 ADR-DESC-02 固定 |
| 数据流与控制流 | 纯只读仓库/工作区查询，无持久化状态写入 | 无并发写冲突 |
| 性能与容量 | 复用现有 BFS/Dijkstra 与 `status` 检查，不引入额外全仓扫描 | 继承 GC-10 |
| 可靠性与容错 | `apply_dirty_mark` 在 `--broken` 模式下显式区分 stat-only/干净、脏工作区（`-dirty`）与损坏工作区（`-broken`） | 在 `DESC-08`（stat 刷新与脏标记）与 `DESC-08a`（检查失败标记）处理 |
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
| 2026-11-17 05:28:00 UTC | Codex R25b FAIL | 在 `DEP-DESC-DOCS` 补充 `/Volumes/Data/gitmono/libra-backend` 缺失时向协调者确认门与 `sync-libra-command-docs.py --check` | N/A | 依赖表、Review log |
| 2026-11-17 05:33:00 UTC | Codex R26 FAIL（Claude R26 取消） | 在 `DEP-DESC-DOCS` 补齐最终网站文档变更的 Claude Code `PASS` 门禁（`cf` 提交、推送、部署及卡完成前置） | N/A | 依赖表、Review log |
| 2026-11-17 06:53:00 UTC | Claude & Codex R27 FAIL | ① `DEP-DESC-DOCS` 按命令分流 `--write --only` / `--check --only`（`describe` vs `rev-parse`），补齐 `.libra`/`.git` 单一 VCS 核验并修正 `AGENTS.md:135` 归属；② `DEP-DESC-TC` 强制唯一 `TC_RUN_ID` 并记录二进制 SHA-256 与 `HEAD` SHA；③ D 组核对实际 `jobs[].name`（`compat-*`/`security-codeql-*`）与 `release.yml` 4-leg matrix/event/ref；④ `DEP-DESC-UTIL` 与同日计划表修正并补全 `branch`/`clone`/`merge-pull`/`global-init-infra` 写集协调；⑤ `DEP-DESC-541` 允许 `DESC-05`/`04` 完成 C/D 组（`Acceptance=complete`）仅保留 `TC-1043`/`1045` 与关单为 `Lifecycle=blocked`，并设 14 天超时升级策略；⑥ `DESC-01` AC8 收窄至普通 `--contains`（`8/8`），`DESC-09` 拆分全长/缩写校验并移除冗余 `~1` AC（`8/8`）；⑦ 同步 `DEFER-DESC-02`（`GII-02`）、`Phase 0` `plan-status.md` 登记门与 `DESC-10` 追溯表测试函数名 | N/A | 依赖表、同日计划表、全局默认、Phase 0/1、`DESC-01/04/05/08/09`、测试矩阵、追溯表、里程碑、风险登记、`DEFER-DESC-02`、完成判据、Review log |
| 2026-11-17 07:42:48 UTC | Codex R28 FAIL（Claude R28 `ECONNRESET`，随 R29 重跑） | `DEP-DESC-541` 不再允许 `DESC-04`/`DESC-05` 在 TC 未通过时进入 C/D 或 `Acceptance=complete`；Phase 1、M1、风险表与完成判据改为 `#541` 合入且 `TC-<id> PASS` 之后才发布 | N/A | 依赖表、Phase 1、里程碑、风险表、完成判据、Review log |
| 2026-11-17 08:05:51 UTC | Codex R29 FAIL | Phase 1 退出条件补入 `DESC-06` `done/complete`，并拆开 `TC-1045`（`DESC-05`）与 `TC-1043`（`DESC-04`）的前置 | N/A | Phase 1、M1、`DEP-DESC-541`、Review log |
| 2026-11-17 08:28:29 UTC | Codex R31 FAIL + Claude R32 FAIL | 新增 `DEP-DESC-COMPAT` 并接入 `DESC-01`..`DESC-10` 的依赖、Granularity 与审计表；`DESC-01` 审计轴去掉尚未实现的 `--broken` | N/A | 依赖表、全部任务卡、审计表、Review log |
| 2026-11-17 08:45:00 UTC | Claude R34 FAIL（Codex R34 PASS，因本修订失效） | `DEP-DESC-UTIL` 纳入 `plumbing-misc` `PM-05`/`PM-06`；`DESC-01..08` 写集补 `docs/development/commands/describe.md`，`DESC-09`/`10` 补 `docs/development/commands/rev-parse.md`；`DESC-01..08` 的 `Full-suite trigger` 改为版本面 `T-1` | N/A | 关系表、依赖表、写集、Review log |
| 2026-11-17 12:01:37 UTC | Codex R35 部分检查与用户单审指令 | R35 仅完成局部检查，无新增 P1，但不作为完整结论；计划提交门改为 Codex 单审 | N/A | 头部、`DEP-DESC-DOCS`、完成判据、Review log |
| 2026-11-17 12:27:28 UTC | Codex R36 FAIL | Phase 0 改为 Codex 单审；区分计划单审与网站交付门，恢复 `cf` 提交/推送/部署前独立 Claude Code PASS；同步文头最近修订时间 | N/A | 头部、Phase 0、`DEP-DESC-DOCS`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R37 部分检查 | R37 未完整复审且无新增 P1；将现行合同与写集网站路径统一为 `/Volumes/Data/gitmono/libra-backend`，取消其它 checkout 回退，保留历史评审记录原文 | N/A | `DEP-DESC-DOCS`、全部现行网站写集、Review log |
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
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（用户最新指令指定；`AGENTS.md` 的 `/Volumes/Sea/libra-backend` 已被取代且本机不存在），并在 `DEP-DESC-DOCS` 写明权威依据与 STOP 条件 | N/A | 全文对应章节 |
| 2026-11-18 09:43:54 UTC | Codex 复审 FAIL 修复（验证契约与门禁；原行误记为未来时间 `2026-11-18 10:35:00 UTC`，R48 按文件 mtime 更正） | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 15:23:09 UTC | R48 预审清扫（用户 2026-11-18 双审指令） | ① 计划文本提交门改为 Claude（Subagent）与 Codex 对同一字节双审均 PASS（头部、Phase 0、M0、完成判据），执行期 ER-05 与网站 Claude Code 门不变；② 网站路径依据改为用户 2026-11-18 11:55:09 UTC 确认，`AGENTS.md:135` 登记为已知文档滞后（本计划不改）；③ 以 pinned Git `2.56.0.116.g6de20f6092` 实测把 describe 后缀语义固化为 `ADR-DESC-03`（字面优先、`<ref>-<count>-g<hex>` 语法、commit 优先消歧与静默失败、tag 对象不剥皮）并新增共享夹具 `FX-DESC-RP`（含离线计算并经 Git 复核的 SHA-1 歧义常量），据此改写 `DESC-09b`（原「歧义返回标准歧义错误」与 Git 不符）；④ `DESC-10..10e` 改为「commit + 消歧」，`DESC-10f..10k` 由「附注 tag 对象」改为「非提交对象（tree、blob、附注 tag）」，消除原 `Verification` 中未计数的隐含判据（tag 暂拒、缺失/歧义）；⑤ `--contains` 忽略 `--dirty` 自 `DESC-01` 移至 `DESC-07`（原 `DESC-01` 轴含两轴），`DESC-01` 补 `describe_test.rs:555` 既有断言；⑥ 重写「验证执行协议」（原协议与命令自相矛盾：`cargo test` 命令挂 nextest 判据、模块过滤要求恰好 1 匹配、`TC` 脚本挂 nextest 判据），新增上一行声称已加但正文并不存在的「文档与网站验收命令」7 步，并把每卡「AC → 断言映射」由机械占位（如「stdout 含 `#795`」）改为可观测期望；⑦ 失败类 AC 只断言稳定错误码；⑧ 网站构建生成产物纳入全部卡写集；⑨ `DEP-DESC-UTIL` 按同批计划当前卡 ID 更新（`BR-40`、`CL-28`、`GII-02/03/04/12/23`、`PM-18` 等），`DEFER-DESC-02` 承接改为 `GII-02`、`GII-24`；⑩ 新增 `DEFER-DESC-05`（大写十六进制后缀）；⑪ 新增用例一律不加 `#[serial]`，不改 `tests/SERIAL_REGISTRY.tsv`；⑫ `src/cli.rs` 锚点 `:3954` → `:4004`；⑬ 上一行（原误记 `10:35:00`）时间按文件 mtime 更正，`R32` Pending 行订正为未执行 | `DESC-10f..10k` 轴变更（tag → 非提交对象）；`--contains --dirty` 判据 `DESC-01` → `DESC-07` | 头部、文档职责、适用范围、关系表、ADR、执行检查必备需求、依赖表、Phase、全部任务卡、审计表、测试矩阵、追溯表、里程碑、风险、收口、延后项、完成判据、Review log |
| 2026-11-18 15:31:27 UTC | 双审 R48：Claude FAIL（3×P1、2×P2）、Codex FAIL（A/B/C/D 四分区共 14 条 P1），受审 SHA-256 `485eb8fee7d4…`；据此修订为 R49 受审字节 | ① `ADR-DESC-03`/`DESC-09` 的 `<ref>` 校验改为 `util::is_valid_refname(&format!("refs/{ref}"))`（原直接调用会拒绝 `v1`/`x` 等单层名）；② 删除全部失败类 stderr 文案 AC（`is ambiguous` 缺席、`DESC-07`/`DESC-08` 错误文案），失败类只断言稳定错误码，`DESC-09b` 7→5、命名空间 commit 卡 6/7、非提交对象卡 8→7、`DESC-07` 改以 index 损坏验证 `--contains` 不做工作区检查（AC6/AC7）、`DESC-08` 补 `--contains --broken` 损坏 index 判据；③ `DESC-09` 与 `DESC-10..10e` 新增临时「边界用例」断言非提交对象 `LBR-CLI-003`，由 `DESC-09a` 与 `DESC-10f..10k` 解除时删除（`ADR-DESC-03` ③）；`DESC-09` 字面优先改以最后一级远程跟踪分支判定并新增非法 `<ref>`（`x.lock`）判据；④ 新增共享夹具 `FX-DESC-MN`（两个 misnamed tag，未选中者不告警），`DESC-05` AC1 改为完整输出谓词、补 `--all` 与告警集合判据（删除与 `DESC-04` 重复的往返 AC），`DESC-06` 补 misnamed `--no-abbrev` 判据（7→8）；⑤ 夹具写 stdin 改用 `run_libra_command_with_stdin`（`tests/command/mod.rs:417`）；⑥ `DESC-02` AC1/AC4、`DESC-03` AC7 改为具体输出谓词；⑦ 文档门第 1 步以进程替换逐行比对 `COMPATIBILITY.md`（排除目标行），第 6 步推送后逐项比对网站既有无关改动；⑧ 评审结论表「依赖与顺序」「完整性」两行同步 | N/A（无拆卡；`DESC-10f..10k` 判据随②调整） | ADR-DESC-03、共享夹具、验证执行协议、文档与网站验收命令、`DESC-02/03/05/06/07/08/09/09a/09b`、`DESC-10..10k`、审计表、测试矩阵、风险、Review log |
| 2026-11-18 15:41:44 UTC | 双审 R49：Claude PASS（3×P2）、Codex FAIL（A 4×P1、B 1×P1、C 2×P1、D 2×P1），受审 SHA-256 `94787a1e45ad…`；按 Codex 发现修订为 R50 受审字节，并按 G-09 自 `DESC-09` 拆出 `DESC-09d` | ① 语法规则拆为纯函数卡 `DESC-09d`（`split_describe_name`，单测覆盖十六进制计数、缺计数段、少于 4 位、`x.lock`、单独 `@`；Git `check_refname_format` 拒绝单独 `@`），`DESC-09` 接线并以 `@-1-g…` 判定使用该函数；② 临时边界补全类型：`DESC-09` 以两个边界用例分别断言 tag 与 tree/blob（由 `DESC-09c`、`DESC-09a` 各自删除），`DESC-10..10e` 边界用例覆盖 tree、blob、tag，删去与根命名空间重复的「两 commit 歧义」判据；③ 删除「不修改既有测试」类无法验证的表述；④ `DESC-05` 的 `--all` 不再承诺内部名，改为断言保持现有引用名输出并登记 `DEFER-DESC-06`；⑤ `DESC-07` AC6/AC7、`DESC-08` AC8 改为可观测谓词（index 损坏时退出 0 且输出 `<tag>^0`），「不做检查」降为实现指引；⑥ TC 命令先在当前 shell `export TC_RUN_ID` 与 `TC_TMP_ROOT`；⑦ 网站无关改动以 `libra status --porcelain -z --untracked-files=all` 与按类型的内容指纹前后比较；`COMPATIBILITY.md` 基线读取单独捕获退出码；⑧ `DEFER-DESC-05` 增补超长后缀（pinned Git 上限为 `GIT_MAX_HEXSZ`）；⑨ `DESC-07` 证据补 R49 实测（index 损坏下 `--contains --dirty` 输出 `<tag>^0`），共享夹具字面引用示例与各卡对齐 | `DESC-09` → `DESC-09`、`DESC-09d`（自 `DESC-09` 拆出） | 适用范围、缺口、关系表、ADR-DESC-03、共享夹具、验证执行协议、文档与网站验收命令、实施顺序、依赖表、发布窗口、Phase 1、审计表、`DESC-09/09a/09c/09d`、`DESC-10..10k`、`DESC-05/07/08`、全部 TC 命令、测试矩阵、追溯表、里程碑、风险、延后项、完成判据 |
| 2026-11-18 15:50:23 UTC | 双审 R50：Claude FAIL（1×P1、2×P2）、Codex FAIL（A 2×P1、B 3×P1、C 1×P1；D PASS 并留 2×P2），受审 SHA-256 `19484fd347b0…`；修订为 R51 受审字节 | ① 撤回 R50 拆出的未接线纯函数卡 `DESC-09d`（违反 G-02），语法规则并回 `DESC-09`：AC5–AC8 断言缺计数段、非十六进制计数段、`x.lock`、单独 `@` 均 `LBR-CLI-003`，`<hex>` 字符与长度交由既有 `resolve_hash_atom_typed` 校验；② 根命名空间四卡改为 G-08 家族 `REL-DESC-09`（`DESC-09`→`09b`→`09c`→`09a` 本地签名提交），新增唯一发布点 `DESC-11`（`release`，聚合守卫 + 发布说明 + `rev-parse` 网站交付），家族内中间提交不发布，删除根命名空间临时边界用例；③ `ADR-DESC-03` ① 补「`<ref>` 不等于 `@`」并删去「仅加 `refs/` 即等价」的表述，③ 改写分阶段口径；④ 网站无变更分支与完成门分开：只有变更分支选 `M`、评审、提交、推送；⑤ 共享夹具改为以 `C` 的 OID 字符串构造 `<C>^{tree}` 与 `<C>:f.txt`；⑥ `DESC-03` AC6 去掉「保持」表述，改为具体输出谓词；⑦ `DESC-06` AC3 改为两字段均等于 `HEAD` 完整 SHA，夹具写全内容并加 5 位前缀唯一性前置断言；⑧ `DESC-07` AC6/AC7、`DESC-08` AC8 的退出 0 改为执行条件写法 | `DESC-09d` 撤回（并回 `DESC-09`）；新增 `DESC-11`（`REL-DESC-09` 发布点） | ADR-DESC-03、共享夹具、执行检查必备需求、文档与网站验收命令、实施顺序、发布分组（`REL-DESC-09`）、Phase 1、审计表、`DESC-03/06/07/08/09/09a/09b/09c/11`、`DESC-10`（依赖）、测试矩阵、追溯表、里程碑、延后项、完成判据 |
| 2026-11-18 15:55:55 UTC | 双审 R51：Claude PASS（1×P2）、Codex FAIL（A 4×P1、B 3×P1、D 2×P1；C PASS 并留 1×P2），受审 SHA-256 `66a7cf4ad258…`；修订为 R52 受审字节 | ① 家族网站时序：子卡在本卡内完成 `rev-parse` 网站页第 1–5 步（写入、构建、独立 Claude Code `PASS`）并保留为已记录的未暂存改动，提交/推送/部署由 `DESC-11` 承载（满足 ER-06a 同卡同步，仅公开时序后移），子卡写集恢复网站文件与 `DEP-DESC-DOCS`；② 网站无关改动快照纳入文件类型与权限位，并记录重命名/复制的两个路径；③ 测试矩阵把命名空间临时边界用例移入「阶段性（非最终覆盖）」行；④ `DESC-11` AC1 于 bump 后、提交前运行，提交后以 `libra status --porcelain` 为空绑定提交内容，AC2 改为单一固定发布说明句，去掉多余的 `DEP-DESC-COMPAT`；⑤ `DESC-02` 新增 AC6（`--exclude` 在 `--no-match` 之前仍生效）；⑥ `DESC-09b` 新增 AC6/AC7（无字面引用时非十六进制或少于 4 位的 `<hex>` 以 `LBR-CLI-003` 失败）；⑦ 共享夹具新增「新增提交 `X`」约定（写文件、`libra add`、`libra commit`），`FX-DESC-RP`、`DESC-06/07/08` 夹具补 `libra add`，写明 `SC` 取值；⑧ `DESC-05` AC6 去掉「保持」表述 | N/A | 文档与网站验收命令、依赖表、`REL-DESC-09`、共享夹具、`DESC-02/05/06/07/08/09/09a/09b/09c/11`、测试矩阵、完成判据 |
| 2026-11-18 16:03:40 UTC | 双审 R52：Claude FAIL（1×P1、1×P2）；Codex 因调用路径未规范化（`-o` 输出路径含 `..`）四分区均未运行、无结论，受审 SHA-256 `5830528355f7…`；修订为 R53 受审字节 | ① `DESC-11` AC1 的提交绑定改为：运行前 `libra status --porcelain` 只列出版本面集合与 `Cargo.lock`（否则 STOP，改用只含家族子卡提交的隔离 checkout），提交后为空并记录 `HEAD^{tree}`（原「提交后为空」在常驻他人改动的共享 checkout 中不可满足）；② `DESC-11` 文档 AC 的步号与 Docs impact 对齐（第 1 步只核对 + 家族网站时序第 2–7 步） | N/A | `DESC-11`、Review log |
| 2026-11-18 16:08:59 UTC | 双审 R53：Claude PASS（1×P2）、Codex FAIL（A 3×P1、B 3×P1；C、D PASS），受审 SHA-256 `cbec1dde9e52…`；修订为 R54 受审字节 | ① 家族子卡改为按 ER-06a 在本卡内完成网站第 1–7 步（模板只为具名 `code-only` 家族提供网站公开时序后移，非 `code-only` 不适用），删除「家族网站时序」；子卡 `Rollback mode` 改为 `compensating`（本地代码逆序 `libra revert`，已推送网站以新 `cf` 提交补偿），网站先于家族发布公开的期间登记风险；`DESC-11` 的 Implementation write set 与 Docs impact 改为 `N/A`；② `DESC-11` AC1 以测试前 `H0`、改动路径集合与逐文件 SHA-256 绑定，提交后逐项核对 `HEAD^`、提交文件集合与内容；③ `DESC-11` AC2 改为创建 Release 前对 `NOTES` 的本地核对（B 组），发布后 `gh release view` 复核归 D 组；④ `DESC-02` AC1 改为两个前置 `--match` 后的 `--no-match`；⑤ `DESC-03` 新增 AC8（`--contains --all` 的 `--exclude`），并声明沿用 `wax` glob 语义、登记 `DEFER-DESC-07`（`*` 跨 `/` 与 Git `wildmatch` 的差异）；⑥ `DESC-01` 映射删去「同用例其它断言不变」说明 | N/A | 文档与网站验收命令、依赖表、`REL-DESC-09`、`DESC-01/02/03/09/09a/09b/09c/11`、风险、延后项、完成判据 |
| 2026-11-18 16:12:33 UTC | 双审 R54：Claude PASS（无发现）、Codex FAIL（A 1×P1、1×P2；B 3×P1；C、D PASS），受审 SHA-256 `6ca6827e4ebd…`；修订为 R55 受审字节 | ① 追溯表改为四张子卡各自交付仓库文档与网站、`DESC-11` 只做标记句替换；② 家族子卡在 `docs/commands/rev-parse.md` 与网站页以固定标记句 `S_U` 注明尚未发布，`DESC-11` 发布时替换为含版本号的 `S_R`（新增 AC4/AC5，写集与 `DEP-DESC-DOCS` 相应恢复），回应子卡网站先于版本公开的误导风险；③ `DESC-11` 聚合守卫拆为 B 组（评审前、bump 前）AC1 与 C 组第 ④ 步重跑并内容级绑定的 AC2，发布说明本地核对改为 AC3；④ 改动路径集合中的 `Cargo.lock` 只在工具链刷新确有差异时计入 | N/A | 文档与网站验收命令、`REL-DESC-09`、`DESC-09/09a/09b/09c/11`、审计表、追溯表、风险 |
| 2026-11-18 16:15:42 UTC | 双审 R55：Claude FAIL（1×P1）、Codex FAIL（A 3×P1、B 2×P1、D 1×P1；C PASS），受审 SHA-256 `06b551a47e21…`；修订为 R56 受审字节 | ① `S_R` 改为不含版本号的 `Describe-name resolution (ref-count-ghex) is supported.`：`DESC-11` 在 ER-05 评审前替换仓库 EN 文档（AC4/AC5 改为评审前对工作区文件的 B 组核对，并由 AC2 的提交内容绑定保证发布提交一致），网站第 2–7 步推迟到 D 组 `release.yml` 全绿、二进制可下载之后，D 组未全绿时网站保持 `S_U` 并按 ER-10 前滚；② `DESC-11` AC3 改为评审前的 B 组 `NOTES` 核对；③ `DESC-11` Verification 拆为 5 个独立门（B 组聚合守卫、C 组聚合守卫、发布说明、`S_U` 缺席、`S_R` 存在），`VER=5/12`；④ 按 CLAUDE.md「新增选项配 focused 单元测试」为 `DESC-02`、`DESC-04`、`DESC-06`、`DESC-08` 各增一个纯函数解析层单测（与既有 AC 同一谓词，`VER` 各 +1，写集加 `src/command/describe_tests.rs`） | N/A | 文档与网站验收命令、`DESC-02/04/06/08/11`、审计表、测试矩阵 |
| 2026-11-18 16:19:40 UTC | 双审 R56：Claude PASS（无发现）、Codex FAIL（A 2×P1、B 2×P1、D 2×P1；C PASS），受审 SHA-256 `5c38dd4f4dee…`；修订为 R57 受审字节 | ① 取消 `S_U`→`S_R` 替换：子卡在 EN 文档与网站加入发布前后都成立的固定可用性说明句 `S_A`，`DESC-11` 不再改写文档（Implementation write set 与 Docs impact 回到 `N/A`，AC 减为 3、VER 3），消除「仓库 `main` 先于 Release 宣称已支持」与发布点标记撤回问题；② `REL-DESC-09` 失败恢复按「均未推送 / PR 已推送未合入 / 已合入未发布 / 已发布」四种状态分别规定动作与远端核验，已合入状态按 ER-10 前滚、不回退 `main`；③ 解析层单测改为各卡独立计数的 AC（`DESC-02` AC7、`DESC-04` AC8；`DESC-06` AC5、`DESC-08` AC5 由集成判据改为解析层判据），不再与集成 AC 共称同一谓词；④ `DESC-07` AC2 改为新增错误变体的 `Display` 钉住断言（修改既有 `describe_error_display_pins_each_variant`，写集加 `src/command/describe_tests.rs`，VER 3） | N/A | 文档与网站验收命令、`REL-DESC-09`、风险、`DESC-02/04/06/07/08/09/09a/09b/09c/11`、审计表、测试矩阵、完成判据 |
| 2026-11-18 16:22:41 UTC | 双审 R57：Claude FAIL（1×P1）、Codex FAIL（A 3×P1、B 1×P1、D 1×P1；C PASS），受审 SHA-256 `4dd75f93ef78…`；修订为 R58 受审字节 | ① `DESC-11` AC2 拆为「C 组聚合测试通过」与新 AC3「发布提交与被测内容逐文件一致」，发布说明顺延为 AC4，Verification 增「提交内容绑定」门（AC/VER 均 4/12）；② `REL-DESC-09` 失败恢复改为按「均未推送 / PR 未合入 / 已合入且 tag 与 Release 未创建 / tag 或 Release 已创建」四种互斥状态处理，状态①先撤销 `DESC-11` 自身的版本面改动或提交；③ 固定发布说明句限定为「无命名空间前缀的 describe 名」，并说明 `tags/`、`heads/` 等前缀尚未支持；④ 完成判据的「最终代码树」定义为 `docs/development/plan/` 之外的被跟踪文件，以 `libra diff --old <HT> --new HEAD --name-only` 判定其后提交只改计划记录，消除记录门结果引发的无限重跑；⑤ `DESC-08` AC5 恢复为脏检查失败时 `--broken=-bk` 输出 `v1.0-bk` 的端到端判据，解析层单测改为 AC7，删去与 AC8 同类的 `--contains` 脏工作区判据 | N/A | `REL-DESC-09`、`DESC-08`、`DESC-11`、审计表、完成判据 |
| 2026-11-18 16:26:41 UTC | 双审 R58：Claude PASS（1×P2）、Codex FAIL（A 1×P1、D 1×P1；B、C PASS），受审 SHA-256 `7bafd14bcb86…`；修订为 R59 受审字节 | ① `REL-DESC-09` 失败恢复状态①改为按已记录的实际进度只处理已存在的 `DESC-11` 改动/提交、已创建的子卡提交与已推送的网站提交，并核验撤回结果；② `DESC-08` 新增 AC3：脏检查失败时 `--broken --json` 的 `data.dirty_mark` 为 `-broken`（GC-04 JSON 分别验收），原 `--dirty=-dm --broken` 判据改为 Description 中「沿用既有 `--dirty` 标记逻辑」的说明；`ADR-DESC-02` 明确 `--broken` 与 `--dirty` 共用 `apply_dirty_mark` 写入 `result`/`dirty_mark` | N/A | `REL-DESC-09`、`ADR-DESC-02`、`DESC-08` |
| 2026-11-18 16:29:35 UTC | 双审 R59：Claude PASS（1×P2）、Codex FAIL（A 2×P1、D 1×P1；B、C PASS），受审 SHA-256 `ba57aa55a8e5…`；修订为 R60 受审字节 | ① `REL-DESC-09` 失败恢复补齐子卡网站的未推送残留（已写入/已暂存：核对写后 SHA-256 后 `libra restore`；已提交未推送：`libra revert` 后一并推送使本地与 `origin/cf` 一致），并以 `DESC-09` 第 3 步记录的修改前 SHA-256 作网站基线；② 完成判据的全量收口门改在只含本计划交付提交、`status` 为空的隔离 checkout 执行，最终代码树判定同时检查提交差异与未提交的被跟踪改动；③ `DESC-08` AC2 改为 `--dirty=-dm --broken` 在真实脏时输出 `v1.0-dm`，AC7 改为新增纯函数 `worktree_suffix` 在未给 `--dirty` 时返回默认 `-dirty` 的单元判据（替代原解析层单测；自定义 `--broken=<mark>` 由 AC5 端到端覆盖），Description 删去「不另设判据」 | N/A | `REL-DESC-09`、`DESC-08`、测试矩阵、完成判据 |
| 2026-11-18 16:33:29 UTC | 双审 R60：Claude PASS（无发现）、Codex FAIL（A 2×P1、D 2×P1；B、C PASS），受审 SHA-256 `3b4f2c93bed3…`；修订为 R61 受审字节，并按 G-09 自 `DESC-08` 拆出 `DESC-08a` | ① `REL-DESC-09` 在 `DESC-09` 开工时、任何实现工作之前无条件记录家族基线（网站页 SHA-256 与 `origin/cf` SHA），恢复终验以此为准；网站「已写入未暂存」须暂存区为空、「已暂存未提交」须暂存路径集合恰为该文件且暂存内容与工作区一致，任一不符即 STOP；② `DESC-08` 判据超过 G-03 上限（默认 `-dirty` 端到端、显式 `-dm`、`--contains` 带/不带 commit 等），按 G-09 拆出 `DESC-08a`（脏检查失败时追加 `<mark>`：AC 为 `-broken`、`-bk`、JSON `dirty_mark`、`worktree_suffix` 的 `Failed` 分支单测）；`DESC-08` 保留 stat-only、默认/显式脏标记、`<commit>` 拒绝、`--contains` 带/不带 commit、`worktree_suffix` 默认标记单测与「检查失败仍 `LBR-IO-001`」临时边界（由 `DESC-08a` 删除） | `DESC-08` → `DESC-08`、`DESC-08a`（自 `DESC-08` 拆出） | `REL-DESC-09`、实施顺序、发布窗口、Phase 2、`DESC-08/08a`、审计表、测试矩阵、追溯表、M2、完成判据 |
| 2026-11-18 16:42:29 UTC | 双审 R61：Claude FAIL（1×P1、2×P2）、Codex FAIL（A 1×P1、B 1×P1、C 2×P1、D 2×P1），受审 SHA-256 `50246c5a944e…`；修订为 R62 受审字节，按 G-09 自 `DESC-01` 拆出 `DESC-01a`，并把 `DESC-10..10k` 并入家族 `REL-DESC-09` | ① `DESC-10..10k` 并入 `REL-DESC-09`（16 张子卡，`DESC-11` 移至 `DESC-10k` 之后统一发布）：命名空间中间态只存在于本地子卡提交、不对外发布，删除六个 commit 卡的临时边界 AC 与边界用例（消除「全长非提交 OID 未覆盖」与「未接入前缀无边界」两类缺口）；`DESC-11` 聚合守卫扩为 16 条、发布说明固定句改为含六种前缀；`ADR-DESC-03` ③ 与 Consequences 同步；② `DESC-01` 的 `^0` 与 `data.exact_match` 改按种子状态（Dijkstra 权重 `weight == 0`）判定，不得用 `generation == 0`（非首父边亦置 0，`describe.rs:518`），新增附注 tag 指向合并提交时第二父为 `v2.0^2` 且 `exact_match == false` 的判据；`--exact-match`/`--candidates 0` 分支按 G-09 拆出 `DESC-01a`；③ 测试矩阵把 `broken_check_failure_boundary` 移入阶段性行，验证执行协议的删除例外改指 `DESC-08a`；④ `FX-DESC-MN` 逐步写明四个提交的文件、暂存与提交命令，`DESC-08a` 夹具在卡内写全；⑤ 非功能需求可靠性行补 `DESC-08a` | `DESC-01` → `DESC-01`、`DESC-01a`（自 `DESC-01` 拆出）；`DESC-10..10k` 由独立发布改为 `REL-DESC-09` 子卡 | `REL-DESC-09`、`ADR-DESC-03`、实施顺序、发布窗口、Phase 1、`DESC-01/01a/02/08a/10..10k/11`、`FX-DESC-MN`、验证执行协议、审计表、测试矩阵、追溯表、风险、M1、完成判据 |
| 2026-11-18 17:05:59 UTC | 双审 R62：Claude PASS（2×P2）、Codex FAIL（A 1×P1、B 1×P1、C 1×P1；D PASS），受审 SHA-256 `012ff693841f…`；修订为 R63 受审字节，新增 `ADR-DESC-04` | ① `DESC-01a` 改变公开 CLI 输出，规模由 `S` 改为 `M`（模板 G-04 `plan-template.md:490-491`：`S` 不允许公开行为变化），卡内 `Estimated scope`、`Granularity` 与审计表同步；② 家族网站先行交付定案于新增的 `ADR-DESC-04`（R55–R62 曾两次往返）：子卡的中性句 `S_A` 改为明确「尚未发布」的 `S_U`，发布点 `DESC-11` 在 ER-05 评审前删除 EN 文档中的 `S_U`（新 AC5）、在 D 组全绿后同步网站并核验线上不再含 `S_U`（新 AC6；AC/VER 4→6/12），`DESC-11` 的写集、Docs impact、依赖（加 `DEP-DESC-DOCS`）与提交内容绑定路径集合同步，`REL-DESC-09` 的窗口终点、失败恢复状态 ①/③/④ 与验收状态、文档与网站验收命令的家族与发布点条目、风险登记、追溯表与网站交付完成判据同步；ADR 写明依据的模板条款行号与被否决的「改写为首个版本号」「中性句」「不含版本号的已支持句」「子卡网站延后」四案；③ 命名空间接线对齐 pinned Git 跨命名空间字面查找（`ref_rev_parse_rules`，`refs.c:643-651`、`:827-835`；`object-name.c:1131`、`:1136`；R63 以 pinned Git 与已安装 Libra 0.30.207 实测六种前缀夹具）：`ADR-DESC-03` ① 新增「字面查找顺序」——前缀名在本命名空间未命中且符合 describe 语法时先以完整名字依次查 tag、本地分支、远程跟踪引用，再进入后缀 helper；`DESC-10..10e` 各增一条以 `ab(C)` 区分 `P` 与后缀对象的判据（`DESC-10` 5→6/8，`DESC-10a..10e` 4→5/8），`FX-DESC-RP` 字面引用约定同步；不符合 describe 语法的前缀名及 `refs/%s`、前缀名的 `refs/remotes/%s/HEAD` 登记 `DEFER-DESC-08`；④ 追溯表 `DESC-08` 行注明 `broken_check_failure_boundary` 为阶段性用例（R62 Codex A P2）；R62 Codex D 的 P2（`DESC-08a` 未直接断言 JSON `result`）与 Claude 两项 P2 登记为残余风险，不改卡 | N/A | `ADR-DESC-03/04`、共享夹具、文档与网站验收命令、`REL-DESC-09`、审计表、`DESC-01a/09/09a/09b/09c/10..10k/11`、追溯表、风险、延后项、完成判据 |
| 2026-11-18 17:11:20 UTC | 双审 R63：Claude FAIL（1×P1、1×P2）、Codex FAIL（C 1×P1；A、B、D PASS，D 留 1×P2），受审 SHA-256 `f394c0fd54a8…`；修订为 R64 受审字节 | ① （Claude P1）`ADR-DESC-03` ①「字面查找顺序」的远程跟踪级（`refs/remotes/%s`）无判据覆盖：`DESC-10` 新增 AC7——远程 `tags` 的分支 `dwr-1-g<ab(C)>` 指向 `SC` 时 `libra rev-parse tags/dwr-1-g<ab(C)>` 输出 `SC`（R63 以 pinned Git 实测；`DESC-10` 6→7/8），ADR 写明 tag 级、本地分支级、远程跟踪级各由哪些卡覆盖；② （Codex C P1）`refs/heads/` 的现有查找（`resolve_local_branch_atom_typed`，`src/utils/util.rs:2205-2209`）在规范分支缺失后先把完整名字当作本地分支名，先于 Git 的 `refs/tags/%s`：R63 实测完整名字同时为 tag 与本地分支时 pinned Git 返回 tag 目标、已安装 Libra 0.30.207 返回分支目标，属与后缀解析无关的既有字面顺序差异——`ADR-DESC-03` ① 更正为如实描述该差异、不再宣称此处与 Git 顺序一致，登记入 `DEFER-DESC-08`，`DESC-10d` Out of scope 同步；③ （Codex D P2，R62 起重复出现）`DESC-08a` 新增 AC4：index 损坏时 `--broken --json` 的 `data.result == "v1.0-broken"`（原单元层 AC4 顺延为 AC5，4→5/8）；④ （Claude P2）「兼容与文档收口」补核 `docs/commands/rev-parse.md` 与网站页均已不含 `S_U` | N/A | `ADR-DESC-03`、`DESC-10`、`DESC-10d`、`DESC-08a`、审计表、延后项（`DEFER-DESC-08`）、兼容与文档收口 |
| 2026-11-18 17:13:51 UTC | 双审 R64：Claude FAIL（1×P1）、Codex PASS（A、B、C、D 四分区均 PASS；A、C 各 1×P2 与 Claude 的 P1 为同一问题，D 1×P2），受审 SHA-256 `527cab92cdf0…`；修订为 R65 受审字节（只改一处，供双方做差异确认） | `DEFER-DESC-08` 的「原因」列原笼统写「现状 fail-closed、不返回错误对象」，与同行登记的 `refs/heads/<n>` 顺序差异（并存时返回分支目标）矛盾：改为逐项如实表述（非 describe 语法名字 fail-closed；`refs/heads/<n>` 顺序差异返回确实存在的同名引用之一的目标、非后缀对象、现状如此且本计划不改变；`refs/%s` 不可表示；`refs/remotes/%s/HEAD` 需远程名含 `/`）。Codex D 的 P2（`DESC-08a` 未断言 JSON `dirty` 字段）登记为残余风险，不改卡 | N/A | 延后项（`DEFER-DESC-08`） |
| 2026-11-18 17:18:32 UTC | 双审 R65 PASS（记账） | Claude R65 与 Codex R65（均为相对 R64 字节的差异确认轮）对受审 SHA-256 `10e7257d1dda82573adc468232ac6355dce9f7476166f02d1398c851a62b890d` 均 `VERDICT: PASS`；本次只更新头部状态行并新增本行与 Codex review log 两行，计划正文未改。残余 P2（不改卡）：Codex R65——`DEFER-DESC-08` 原因列「非 describe 语法名字现状以 `LBR-CLI-003` 失败」未排除 `refs/heads/<n>` 完整名字分支回退命中的情形；Codex R64 D——`DESC-08a` 未断言 JSON `dirty` 字段；Claude R62——家族窗口扩至 16 张子卡的执行成本、`DESC-01` 与 `DESC-01a` 之间一个版本的 `--contains`/`--contains --exact-match` 输出不一致（文档已说明） | N/A | 头部状态、修订历史、Codex review log |

## 已决议设计决策

### ADR-DESC-01: 通过 clap `overrides_with` 实现 `--no-long`、`--no-match`、`--no-abbrev` 的 last-wins 语义

- **Status:** Accepted
- **Context:** Git 的 `parse-options` 允许 `--long --no-long`、`--match a --no-match --match b`、`--abbrev=4 --no-abbrev` 在同一命令行出现，后出现的标志覆盖先前设置。本计划不修改 `src/cli.rs` 全局解析逻辑。
- **Decision:** 在 `DescribeArgs` 中全部采用 clap 原生 `overrides_with` 与对称布尔/值标志实现 last-wins，并同步更新 `describe_commit_for_atom`（`src/command/describe.rs:159-173`）的 `DescribeArgs` 结构体字面量默认值。
- **Alternatives considered:** 修改 `src/cli.rs` 注入位置索引，会触发 ER-13 `T-1` 跨切面修改且超出 `describe` 单一模块范围。
- **Consequences:** 无须改动 `src/cli.rs`；各选项的命令行顺序覆盖行为由对应卡的集成用例（`DESC-02`、`DESC-04`、`DESC-06`）按 pinned Git 实测结果验证。
- **Revisit when:** Clap 升级或仓库引入统一的 Git `parse-options` 兼容层时。

### ADR-DESC-02: 保持 `DescribeOutput` JSON 契约向后兼容

- **Status:** Accepted
- **Context:** `libra describe --json` 已有稳定的 `DescribeOutput` 结构（`src/command/describe_types.rs:5-18`）。
- **Decision:** 本计划修复中不重命名或删除任何现有 `DescribeOutput` 字段；`--contains` 的 `^0` 仅反映在 `result`（`tag` 仍为纯 tag 名，`exact_match` 仍为 `true`）；misnamed tag 下 `data.tag` 保留内部 tag 名（如 `"A"`），`data.abbreviated_commit` 为实际使用的缩写/完整哈希（在 misnamed tag + `--abbrev=0` 强制输出后缀时为完整 commit SHA）；`--broken` 的标记与 `--dirty` 共用 `apply_dirty_mark` 写入 `result` 与 `dirty_mark`（`dirty_mark` 为所用标记，如 `-broken`），不破坏现有 JSON 解析器。
- **Alternatives considered:** 修改 `DescribeOutput` 现有字段类型或删除字段，会破坏既有自动化调用方。
- **Consequences:** 所有现有 `--json` 测试持续兼容。
- **Revisit when:** 未来发布主版本重构 JSON schema 时。

### ADR-DESC-03: `rev-parse` 的 describe 后缀解析语义与家族分阶段交付

- **Status:** Accepted（2026-11-18 R48 以 pinned Git 实测复核后修订）
- **Context:** Git 在 `get_oid_1` 中先按字面引用（`get_oid_basic`/dwim_ref）解析，未命中时才调用 `get_describe_name`（pinned 上游 `object-name.c:1028-1081`）：名字须形如 `<ref>-<count>-g<hex>`，其中 `<count>` 为 ≥ 1 个十六进制字符（`isxdigit`）、`<ref>` 须通过 `check_refname_format`、`<hex>` 交给 `get_short_oid`（≥ 4 位），并以 `GET_OID_QUIETLY | GET_OID_COMMIT` 解析；`<ref>` 只作语法校验，其内容（`v1`、`tags/x`、`heads/x`、`remotes/o/x`、`refs/{tags,heads,remotes}/...` 或任意合法 ref 名）不参与对象查找。2026-11-18 以 pinned Git 构建（`/Volumes/Data/tmp/libra-testcases/git-build-6de20f6092`，`git version 2.56.0.116.g6de20f6092`）在临时仓库实测：`x-1-g<8位 commit>`、`nonsense-1-g<8位 blob>`、`heads/x-1-g<8位 tree>`、`tags/q-3-g<8位 tag 对象>`、`remotes/o/x-1-g<40位 tag 对象>` 均返回该对象自身（tag 对象不剥皮）；`x-1a-g<8位>`（计数段含十六进制字母）与 `tags/x-1-g<40位 commit>~1`（返回父提交）成功；`foo-g<hex>`、`x--g<hex>`（无计数段）、`x.lock-1-g<hex>`（`<ref>` 非法）、`v1-1-g<3位>`、不存在对象的缩写或全长后缀均失败；歧义前缀中**恰有一个 commit** 候选时返回该 commit（`x-1-g92ae10d0`、`tags/x-1-g92ae10d0`、`refs/remotes/o/x-1-g92ae10d0` 均返回 commit `92ae10d0…`），两个 blob 或两个 commit 共享前缀时失败且只报 `unknown revision`，不输出 `short object ID … is ambiguous` 候选提示（而裸 `f6a3ecac` 会输出该提示）。字面引用查找由 `repo_dwim_ref` → `expand_ref`（pinned `refs.c:827-835`）按 `ref_rev_parse_rules`（`refs.c:643-651`：`%s`、`refs/%s`、`refs/tags/%s`、`refs/heads/%s`、`refs/remotes/%s`、`refs/remotes/%s/HEAD`）逐条尝试，`get_oid_basic`（`object-name.c:1131`）先于 `get_describe_name`（`object-name.c:1136`）；故带前缀的名字在本命名空间引用缺失时，仍会命中以**完整名字**命名的 tag、本地分支或远程跟踪引用（如 `tags/x-1-g<hex>` 命中 `refs/heads/tags/x-1-g<hex>`）。2026-11-18（R63）以同一 pinned 构建实测：本地分支 `tags/dw-1-g<C 的 8 位>`、tag `heads/dw-1-g<…>`、本地分支 `remotes/o/dw-1-g<…>`、本地分支 `refs/tags/dw-1-g<…>`（即 `refs/heads/refs/tags/…`）、tag `refs/heads/dw-1-g<…>`（即 `refs/tags/refs/heads/…`）、本地分支 `refs/remotes/o/dw-1-g<…>` 分别指向 `P` 时，`rev-parse` 同名全部返回 `P`，而无此类引用的对照名 `tags/zz-1-g<…>` 返回后缀对象 `C`；另以远程 `tags`（`git remote add tags <S>` 后 fetch 得 `refs/remotes/tags/dwr-1-g<…>` 指向 `SC`）实测 `rev-parse tags/dwr-1-g<…>` 返回 `SC`；已安装 Libra 0.30.207 可用 `libra branch`/`libra tag` 创建这六个引用，现状对这六个名字均以 `LBR-CLI-003` 失败。Libra 现状：`resolve_object_atom_typed`（`src/utils/util.rs:2404-2468`）对 `tags/`、`heads/`、`remotes/`（含 `refs/` 形式）早返回，其余名字依次尝试 tag、本地分支、远程分支，最后才走纯十六进制 `resolve_hash_atom_typed`（`src/utils/util.rs:2319-2393`，缺失返回 `InvalidReference`，歧义返回带候选的 `Ambiguous`，二者经 `rev_parse_target_error` 均映射 `LBR-CLI-003`，`src/command/rev_parse.rs:997-1010`）。
- **Decision:** ① 新增一个共享 helper，在字面引用全部未命中后（判定见本条末尾的「字面查找顺序」），按上述 Git 语法（`<count>` 为十六进制字符、`<ref>` 须非空、不等于 `@`（pinned `refs.c:303-308` 对单独 `@` 的特判），且 `util::is_valid_refname(&format!("refs/{ref}"))` 为真（该函数只接受 `HEAD` 与 `refs/` 前缀，`src/utils/util.rs:4180-4212`；前置 `refs/` 使 `v1`、`x`、`tags/x` 合法而 `x.lock` 非法，但它本身接受 `refs/@`，故需另判 `@`）；`<hex>` 的字符与长度交由既有 `resolve_hash_atom_typed` 校验（`src/utils/util.rs:2319-2335`：非十六进制或长度不在 4..=当前 hash 长度即 `InvalidReference`，与 Git 对少于 4 位或非十六进制后缀失败一致；大写与超过当前 hash 长度的情形见 `DEFER-DESC-05`））做后缀解析；不匹配该语法时保持原有错误。**字面查找顺序（对齐 pinned `ref_rev_parse_rules`）：** 无前缀的根名字即现有三级查找（tag、本地分支、远程跟踪分支，`src/utils/util.rs:2457-2466`）；带 `tags/`、`heads/`、`remotes/`、`refs/tags/`、`refs/heads/`、`refs/remotes/` 前缀的名字先做该命名空间自身的现有查找（`src/utils/util.rs:2438-2455`；其中 `refs/heads/` 的现有查找在规范分支缺失后还会把完整名字当作本地分支名再查一次，`resolve_local_branch_atom_typed`，`src/utils/util.rs:2205-2209`，即先于 Git 的 `refs/tags/%s` 检查 `refs/heads/%s`——这是既有字面解析与 Git 的顺序差异，只在完整名字 `refs/heads/<n>` 同时被 tag 与本地分支使用时可见：R63 实测 pinned Git 返回 tag 目标并告警 `refname … is ambiguous`，已安装 Libra 0.30.207 返回分支目标，与后缀解析无关；本计划不改该现有查找，登记 `DEFER-DESC-08`），未命中且名字符合 describe 语法时，再以**不剥前缀的完整名字**依次作为 tag 名、本地分支名、远程跟踪名查找（对应 `refs/tags/%s`、`refs/heads/%s`、`refs/remotes/%s`；实现指引：`tag::find_tag_ref(<完整名>)`，`src/internal/tag.rs:431`；`resolve_branch_commit_typed(<完整名>, None, …)`，`src/utils/util.rs:2052`；`remote_tracking_candidates(<完整名>)`，`src/utils/util.rs:2077`——不得改用会剥去 `refs/tags/` 等前缀的 `resolve_*_atom_typed`），命中即返回该引用的目标，全部未命中才进入后缀 helper；`DESC-10..10e` 各以一条区分 `P` 与后缀对象的判据覆盖本前缀（三级中 tag 级由 `heads/`、`refs/heads/` 两卡，本地分支级由其余四卡，远程跟踪级另由 `DESC-10` 一条判据覆盖）。不符合 describe 语法的前缀名字保持现有命名空间查找（未命中即 `LBR-CLI-003`，不返回错误对象），以及 `refs/%s`（Libra 引用只有 tag、分支、远程三类，无法表示 `refs/refs/…`）与前缀名的 `refs/remotes/%s/HEAD` 两条规则，登记为 `DEFER-DESC-08`。② 后缀解析采用 Git 消歧：唯一候选返回该对象；多个候选中恰有一个 commit 时返回该 commit；其余歧义与缺失一律以 `LBR-CLI-003` 失败（实现上返回 `InvalidReference`、不输出歧义候选提示以贴近 Git 的静默失败；该提示文案不设验收判据，失败类判据只看稳定错误码）。③ 分阶段交付：根命名空间与六种前缀命名空间统一以 G-08 家族 `REL-DESC-09` 交付——`DESC-09`（语法与 commit 解析）→ `DESC-09b`（② 的缺失与消歧）→ `DESC-09c`（附注 tag 对象，返回其自身）→ `DESC-09a`（blob 与 tree）→ `DESC-10..10e`（依次让 `tags/`、`heads/`、`remotes/`、`refs/tags/`、`refs/heads/`、`refs/remotes/` 的字面查找未命中时进入同一 helper 解析 commit，并带 ② 的消歧语义）→ `DESC-10f..10k`（依次为上述前缀放开 tree、blob 与附注 tag）均只本地签名提交，由发布点 `DESC-11` 一次对外发布；家族内的类型与前缀放行顺序只是实现顺序，中间提交不对外发布，故不设临时边界判据。④ 本计划只验证 SHA-1 仓库夹具；SHA-256 仓库沿用 `resolve_hash_atom_typed` 按 `get_hash_kind()` 的长度判定，不另设判据。大写十六进制后缀（Git 接受）登记为 `DEFER-DESC-05`。
- **Alternatives considered:** 一次性实现全部对象类型与命名空间（判据远超 G-03 上限，被历史评审要求拆分）；对歧义返回 Libra 标准 `Ambiguous` 候选提示（与 Git describe 名的静默失败及 commit 优先消歧不一致，R48 实测否决）。
- **Consequences:** 各卡只扩展 helper 的入口（含前缀名的跨命名空间完整名字查找）或类型放行，单一事实源在 helper；家族中间态只存在于本地子卡提交、不对外发布，`DESC-11` 发布的版本即完整行为，不存在对外的临时边界。
- **Revisit when:** 仓库引入统一的 Git `get_oid` 兼容层或 `core.disambiguate` 支持时。

### ADR-DESC-04: 家族 `REL-DESC-09` 网站先行交付的「尚未发布」说明与发布点删除

- **Status:** Accepted（2026-11-18 R63 定案。R54 修订曾用「尚未发布」句 + 发布点改写为含版本号的句子，R55 修订改为不含版本号，R56 修订改为发布前后都成立的中性句 `S_A`，R62 Codex B 又以 `S_A` 不说明尚未发布判 P1；本 ADR 记录最终选择、模板依据与被否决方案，后续评审以本 ADR 为准）
- **Context:** ① 本计划不属模板 v2.16 `code-only` 具名范围（`plan-template.md:57`），「家族 child 的网站提交、`cf` push 与部署由集成点承载」的公开时序后移只在该具名范围内优先于 ER-06a（`plan-template.md:66`），故本计划家族子卡须按 ER-06a 在本卡内交付并部署网站文档（`plan-template.md:330`）；② 家族子卡只本地签名提交、不推送，代码由唯一发布点统一发布（`plan-template.md:253`、`:499`），子卡期间网站描述的能力不在任何已发布版本中；③ ER-05 评审先于 C 组（`plan-template.md:248`），版本号在 C 组第 ② 步 bump 时才确定，C 组先提交并合入 PR（第 ⑦–⑧ 步）再在合入提交上 `gh release create`（第 ⑨ 步，`plan-template.md:307`），release artifact 的编译上传与可下载属 D 组（`plan-template.md:319`）；④ 网站页由 `scripts/sync-libra-command-docs.py` 自 EN 仓库文档生成，网站只有 EN 页（`apps/tanstack-app/content/docs/commands/*.en.md`）。
- **Decision:** 子卡阶段：`DESC-09` 在 EN `docs/commands/rev-parse.md` 的 describe 名段落加入单独成行的固定句 `S_U` = `Describe-name resolution (ref-count-ghex) is not yet included in any published Libra release.`，后续子卡保留，经各子卡同卡网站交付公开，准确表述「尚未发布」。发布点阶段：`DESC-11` 在 ER-05 评审前（B 组）删除 `S_U` 所在行（`DESC-11` AC5），该删除与版本面一起进入发布提交（AC3 的提交内容绑定）；网站在 D 组全绿（`release.yml` 全部 job 成功、三个下载地址 HTTP 200）之后才同步，并核验线上页面不再含 `S_U`（AC6），D 组未全绿时网站保持 `S_U`。首个支持版本由 GitHub Release（标题 `v${VERSION}`，发布说明含 AC4 固定句）承载，不写入命令文档。zh-CN 与开发文档不加 `S_U`：它们只随子卡本地提交，`DESC-11` 合入前不公开，网站也无对应页面。
- **Alternatives considered:** (a) 发布前后都成立的中性句 `S_A`（R57–R62 受审字节）：读者无法得知该能力尚未发布（R62 Codex B P1），否决。(b) 发布点把说明改写为含首个支持版本号的句子（R55 受审字节；R62 Codex B 的建议）：版本号只在 ER-05 评审之后的 C 组第 ② 步确定（`plan-template.md:248`、`:307`）——评审前写入须预测版本，而 ER-12 要求单一发布者在进入发布前才重新读取 `Cargo.toml` 权威版本（`plan-template.md:363-365`），其间上游版本前移会使预测落空；评审后写入则发布提交含未经评审的文档改动（R55 Codex 已判 P1）；模板只把 `compat_version_surface_sync` 报告的版本面集合列为 bump 时的机械同步对象，另设「文档内版本号」属自创流程；否决。(c) 改写为不含版本号的「已支持」句（R56 受审字节）：与文档正文重复，且须另设标记撤回规则，否决。(d) 子卡网站延后到发布点推送：模板只对具名 `code-only` 家族提供（`plan-template.md:66`），本计划不适用（R53 Codex P1），否决。
- **Consequences:** 仓库 `main` 自 `DESC-11` 合入起不含 `S_U`、只描述同一提交中的代码行为；`main` 先于 `gh release create` 获得文档是模板 C 组第 ⑧→⑨ 步对所有发布卡的固有顺序（`plan-template.md:307`），本计划每张 `independent` 卡同样如此，不构成本家族特有缺陷（R56 Codex 曾就「`main` 先于 Release 宣称已支持」判 P1，删除方案不在 `main` 作任何发布状态声明，该顾虑不再成立）。网站自子卡交付起至 `DESC-11` 的 AC6 完成止持续显示 `S_U`：其中自 `gh release create` 到网站同步完成的间隔偏保守地低报、不会高报。`DESC-11` 由此有文档写集与两条计数判据（AC5、AC6，`release` 卡上限 12 内）；家族失败恢复状态 ① 一并恢复 `DESC-11` 的 EN 文档改动。
- **Revisit when:** 模板为非 `code-only` 家族提供网站公开时序后移，或网站支持按版本区分的文档。

**共享测试夹具（各卡集成用例按名引用；每个用例自建独立临时仓库，不共享状态）：**

「新增提交 `X`」约定（各卡夹具通用，除非卡内另写明文件与内容）：写入新文件 `X.txt`（内容 `X\n`），`libra add X.txt`，`libra commit -m X --no-verify`（同 `tests/command/describe_test.rs:665-670` 写法），`X` 的 OID 以 `libra rev-parse HEAD` 读出；`create_committed_repo_via_cli()`（`tests/command/mod.rs:696`）只创建并提交 `tracked.txt`。

`FX-DESC-RP`（`DESC-09`、`DESC-09a..09c`、`DESC-11`、`DESC-10`、`DESC-10a..10k` 共用）：

- **基础对象：** `create_committed_repo_via_cli()` 新建 SHA-1 仓库；写 `f.txt` 内容 `rp-1\n`、`libra add f.txt`、`libra commit -m P --no-verify` 得 `P`，再写 `rp-2\n`、`libra add f.txt`、`libra commit -m C --no-verify` 得 `C`（`HEAD` 在本地分支上；各 OID 以 `libra rev-parse HEAD` 读出）；`T` = `libra rev-parse <C>^{tree}`、`B` = `libra rev-parse <C>:f.txt`（`<C>` 为上一步读出的 `C` 完整 OID 字符串，测试中以 `format!("{c}^{{tree}}")`、`format!("{c}:f.txt")` 构造参数，不是名为 `C` 的引用）；`libra tag -m v1 v1 <P>` 建附注 tag，`G` = `libra rev-parse v1`（tag 对象自身，`src/utils/util.rs:2172-2198` 不剥皮）。`ab(X)` = `X` 的前 8 位十六进制（夹具对象少，8 位唯一；用例内以 `libra rev-parse <ab(X)>` 返回 `X` 作前置断言）。
- **远程命名空间：** 另建仓库 `S`（同样由 `create_committed_repo_via_cli()` 创建，`SC` = 在 `S` 中执行 `libra rev-parse HEAD` 的输出；各卡在 `S` 中建分支时一律指向 `SC`），在主仓库执行 `libra remote add o <S 路径>` 与 `libra fetch o`，得到 `refs/remotes/o/<S 的分支>`；后缀名形如 `remotes/o/x-1-g<hex>`、`refs/remotes/o/x-1-g<hex>`。
- **字面引用：** 字面名的后缀十六进制取另一个已存在对象（`B`）的前缀，使「先解析后缀」的错误实现得到不同 OID；具体名字由各卡写明（轻量 tag `tlit-1-g<ab(B)>` 与本地分支 `blit-1-g<ab(B)>` 指向 `P`，`S` 中的分支 `rlit-1-g<ab(B)>`、`rbad-1-gxyz` 指向 `SC`）。跨命名空间完整名字（`ADR-DESC-03` ①「字面查找顺序」）的引用名取 `<前缀>dw-1-g<ab(C)>` 形式、指向 `P`，由 `DESC-10..10e` 各自写明，使「只查本命名空间即进入后缀 helper」的错误实现得到 `C`。
- **歧义对象（SHA-1 常量，R48 以 Python `hashlib` 离线计算，并用 `git hash-object -w` 写入临时仓库、以 pinned Git `rev-parse` 复核消歧结果）：** 先以 `run_libra_command_with_stdin(&["hash-object", "-w", "-t", "tree", "--stdin"], <repo>, "")` 写入空树 `4b825dc642cb6eb9a060e54bf8d69288fbee4904`。blob 以 `run_libra_command_with_stdin(&["hash-object", "-w", "--stdin"], <repo>, "desc-blob-<i>\n")` 写入；commit 以 `run_libra_command_with_stdin(&["hash-object", "-w", "-t", "commit", "--stdin"], <repo>, "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor D <d@example.com> 1 +0000\ncommitter D <d@example.com> 1 +0000\n\ndesc-commit-<i>\n")` 写入（helper 见 `tests/command/mod.rs:417`）。① 两 blob 共享前缀 `f6a3ecac`：`i=4843` → `f6a3ecacabf784be10454a8bcea4854316e39c68`，`i=11379` → `f6a3ecacd1f44b6efa01bd83b005f7b309c6deeb`；② 两 commit 共享前缀 `c3da9f35`：`i=33527` → `c3da9f357b85fce3558bc5694c616135d02679d5`，`i=80372` → `c3da9f358d9197b811249094f2141162fde05ac3`；③ commit 与 blob 共享前缀 `92ae10d0`：commit `i=64509` → `92ae10d00618309f4ecba5b1735d693bc74ceaf5`，blob `i=193006` → `92ae10d055422ea25e65ed1319a956e2dac419f3`。用例写入后先断言 `libra hash-object -w` 输出等于上列 OID，并断言裸前缀（如 `libra rev-parse f6a3ecac`）以 `LBR-CLI-003` 失败且 stderr 含 `is ambiguous`（前置断言：夹具确实歧义；只验证夹具成立，不是行为判据）。
- **缺失对象：** 缩写缺失用 `00000000`、全长缺失用 40 个 `0`；用例先断言 `libra rev-parse 00000000` 以 `LBR-CLI-003` 失败且 stderr 不含 `is ambiguous`（前置断言：夹具中无此前缀对象）。
- **期望值来源：** 期望 OID 一律来自上述夹具构造（`P`/`C`/`T`/`B`/`G` 由 Libra 读出，歧义常量由内容固定），Git 对同构名字的行为已于 2026-11-18 以 pinned Git 构建实测（见 Context）；执行者在 ER-02 以同一 pinned 构建复核并把结论写入本卡。

`FX-DESC-MN`（misnamed annotated tag；`DESC-05`、`DESC-04`、`DESC-06` 共用）：`create_committed_repo_via_cli()` 后在当前分支依次执行：写新文件 `Cb.txt`（内容 `Cb\n`）、`libra add Cb.txt`、`libra commit -m Cb --no-verify` 得提交 `Cb`，随后 `libra tag -m B B`；写新文件 `Ca.txt`（内容 `Ca\n`）、`libra add Ca.txt`、`libra commit -m Ca --no-verify` 得 `Ca`，随后 `libra tag -m A A`；写新文件 `c.txt`（内容 `c\n`）、`libra add c.txt`、`libra commit -m c --no-verify` 得 `c`；写新文件 `d.txt`（内容 `d\n`）、`libra add d.txt`、`libra commit -m d --no-verify` 得 `d`（各提交 OID 在提交后以 `libra rev-parse HEAD` 读出；分支停在 `d`，故 `c`、`Ca` 不是任何分支的端点）；取 `GA` = `libra rev-parse A`、`GB` = `libra rev-parse B`（tag 对象），再 `libra tag Q <GA>`、`libra tag -d A`、`libra tag R <GB>`、`libra tag -d B`（`src/command/tag.rs:817-869` 保留未剥皮的 tag 对象，不依赖 `update-ref`）。前置断言：`libra tag -l` 的输出行集合恰为 {`Q`, `R`}，`libra rev-parse Q` 恰为 `GA`。pinned Git 对同构夹具（以 `update-ref` 改名）实测：`describe c` → stderr 恰一行 `warning: tag 'Q' is externally known as 'A'`（未选中的 `R` 不告警）、stdout `A-1-g<7 位>`；`--abbrev=0 c` 与 `--no-abbrev c` → `A-1-g<40 位>`；`--all c` → stderr `warning: tag 'tags/Q' is externally known as 'A'`、stdout `tags/A-1-g<7 位>`；`--no-long Ca` → `A-0-g<7 位>`；`--contains Ca` → `Q^0` 且无警告。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 全部 `GC-01..13`：
- **GC-01** 现状核实前置；**GC-02** 单一事实源；**GC-03** Git 互操作边界；**GC-04** 输出与错误契约（`LBR-CLI-002` 等）；**GC-05** 文档同步（`COMPATIBILITY.md:219` 仅原地修改，禁止增删行破坏 `tests/compat-ledger` 固定行号）；**GC-06** 测试索引；**GC-07** 安全默认值；**GC-08** 原子性与恢复；**GC-09** 并发与资源生命周期；**GC-10** 性能预算；**GC-11** 生产 panic 禁止；**GC-12** 精确暂存；**GC-13** 数据库迁移作用域与全局配置隔离（本计划不触及 SQLite schema）。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 全部 `ER-01..14`：
- **ER-01** 开工前 `libra status --short --branch` 检查；**ER-02** 刷新锚点与复现（含以 pinned Git 复核 `ADR-DESC-03` 所列 Git 行为）；**ER-03** 粒度门禁；**ER-04** A/B/C/D 四组验收门；**ER-05** review 闭环；**ER-06 / ER-06a** 同卡 EN/zh-CN/开发文档/网站/`COMPATIBILITY.md` 同步（按下文「文档与网站验收命令」执行）；**ER-07** Libra-native 签名+DCO 提交；**ER-08** 每卡 `patch + 1` + 版本面同步 + 工具链刷新 `Cargo.lock` + `gh release create`；**ER-09 / ER-10** 失败策略；**ER-11** 证据卫生；**ER-12** 串行发布；**ER-13** 执行阶段 focused 测试 + 全量收口门；**ER-14** nextest 作为发布集成测试唯一执行器。
- **ER-04 C 组与合入（每张 `independent` 卡与家族发布点 `DESC-11`；家族 `REL-DESC-09` 的子卡按 G-08 只跑本卡 A 组与 fmt、clippy 并本地签名提交，不 bump、不构建发布、不推送，C/D 覆盖继承 `DESC-11`）：** 本计划不属模板 v2.16 `code-only` 具名范围，每张会发布卡按 ER-08 执行 `patch + 1`、同步 `compat_version_surface_sync` 报告的版本面集合（当前 `Cargo.toml`、`install.sh`、`install.ps1`）、工具链刷新 `Cargo.lock`。因每卡均修改 `install.sh`/`install.ps1`（`T-1`），C 组第 ④ 步除本卡 A 组 focused nextest 外，必须跑双环境全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`、`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 与 `cargo build --release`，随后签名+DCO 提交并推送 PR。因本工作区为 `.libra` 仓库（无 `.git`），PR 以 squash merge 合入时，发布前必须 `libra fetch origin` 同步远端 `main`，记录合入提交 `MERGE_SHA`，核验该提交的 PGP 签名与 `Signed-off-by` DCO trailer（ER-07），并比较树对象：`libra rev-parse '<reviewed-head>^{tree}'` 与 `libra rev-parse "${MERGE_SHA}^{tree}"` 必须逐字相同；`base.yml`/`codeql.yml` 在该精确 SHA 全绿（见 D 组默认）。**止步条件：** 任一核验失败立即 STOP，严禁执行 `gh release create "v${VERSION}"`。

- **验证执行协议（对全计划每条 `Verification` 条目强制）：**
  1. **分类：** 每条 `Verification` 标明三类之一——「卡特有用例」（`(new)` 或本卡修改的既有具名用例，只用精确过滤 `-E 'test(=<完整测试名>)'`，多条用 `or` 并列精确名；`DESC-08a` 按 G-09 拆分删除的临时「边界用例」`command::describe_test::broken_check_failure_boundary` 自该卡起不再运行，删除本身在卡内登记）、「共享回归组」（锚定模块的正则过滤 `-E 'test(/^<模块路径>::/)'`）、「非 nextest 门」（`DEP-DESC-TC` 的 `tc*.sh` 脚本：先在当前 shell `export TC_RUN_ID=… TC_TMP_ROOT=/Volumes/Data/tmp/libra-testcases`，再构建并运行脚本，判据与记录项按 `DEP-DESC-TC`）。测试名形式：集成测试为 `command::<module>::<fn>`（`tests/command_test.rs` 的 `mod command` 与 `tests/command/mod.rs:1039-1040`），单元测试为 crate 内完整模块路径（`utils::util::test::<fn>`，`src/utils/util.rs:4214-4215`；`command::describe::tests::<fn>`，`src/command/describe.rs:671-673`）。
  2. **列举：** 运行任一 nextest 条目前，先以相同 target 与过滤执行 `cargo nextest list`（如 `cargo nextest list --test command_test -E '<同一过滤>'`）并记录列出的测试名：卡特有用例的列出集合必须与条目写明的完整测试名集合逐字相等（单名即恰好 1 条）；共享回归组必须 ≥ 1 条并记录条数。不满足即 STOP。
  3. **运行与判据：** 记录退出码与 nextest 汇总行 `N tests run: P passed, F failed, S skipped`（`N` 为 1 时写作 `1 test run`；某项为 0 时 nextest 可能省略该段，按 0 记）：要求退出码 = 0、`F` = 0、`N` = `P` = 第 2 步列出条数。`S` 是被过滤排除（及未选中的 `#[ignore]`）的测试数，只记录、不作判据。出现 `FLAKY` 须写明，`--retries` 不得掩盖最终失败。卡特有用例必须是 L1 确定性用例（不依赖 L2/L3 环境门，不得以打印 `skipped (...)` 提前返回）。
  4. **AC 绑定：** 每条卡特有用例在所在卡的「AC → 断言映射」中写明覆盖的 AC 编号、场景、期望结果（退出状态、stdout/stderr 关键文本或 JSON 字段、前后状态）；映射只写判定对应 AC 所需的检查，不列 AC 之外的独立断言（凡可独立失败的行为断言都必须是一条计数的 AC）；映射中标注「前置断言」的检查只验证夹具成立，不是行为判据。失败类 AC 只以稳定错误码判定（人读模式用 `parse_cli_error_stderr` 解析 stderr 得到的 `error_code`，`tests/command/mod.rs:516`；`--json` 模式用错误信封的 `error_code`），并另断言被测命令以非 0 退出（执行条件）；数值退出码可被 `LIBRA_FINE_EXIT_CODES` 改变，本计划不以具体数值作判据。
  5. **共享回归组** 属 ER-04 A 组 focused 门，只证明既有行为未回归，不计为卡特有 AC 证据，也不计入 G-03 的 `VER` 分子（条目标注「不计入 G-03 VER」）；判据同第 3 步。
  6. **新增测试的隔离约束：** 本计划新增的集成用例一律以子进程方式实现（`run_libra_command(&argv, <repo path>)`，需写 stdin 时用 `run_libra_command_with_stdin(&argv, <repo path>, <body>)`，`tests/command/mod.rs:417`；均显式 cwd，同 `tests/command/describe_test.rs:7-24` 写法），不加 `#[serial]`；新增单元用例只测纯函数（不读写仓库、不改 cwd/env），同样不加 `#[serial]`。因此本计划不修改 `tests/SERIAL_REGISTRY.tsv` 与 `.config/nextest.toml`；若实现发现必须加 `#[serial(...)]`，先按 ER-03 修订计划把这两个文件及 `sh tests/NEXTEST_GROUPS.sh` 纳入写集与验收，再开工。
  7. **记录：** 命令、列出条数、汇总行、退出码与 UTC 时间写入本计划对应卡的 `Verification` 条目（v2.12，不新建证据文件）。

- **文档与网站验收命令（ER-06/ER-06a 同卡门）：** 除 `DESC-11`（只执行下述「发布点」条目所列步骤）外，每张卡执行第 1–7 步。以下 `<cmd>` 对 `DESC-01..08`、`DESC-01a`、`DESC-08a` 为 `describe`，对 `DESC-09`、`DESC-09a..09c`、`DESC-10`、`DESC-10a..10k`、`DESC-11` 为 `rev-parse`；不得跨命令连带同步无关页面。
  - **家族子卡的网站交付与「尚未发布」说明（`ADR-DESC-04`）：** `REL-DESC-09` 的子卡与其它卡一样在本卡内完成第 1–7 步（ER-06a 同卡交付；模板只为具名 `code-only` 家族提供网站公开时序后移）。因子卡代码在 `DESC-11` 发布前不进入任何发布版本，`DESC-09` 在 `docs/commands/rev-parse.md` 的 describe 名段落加入单独成行的固定句 `S_U` = `Describe-name resolution (ref-count-ghex) is not yet included in any published Libra release.`，其后子卡保留；该句经第 3 步同步到网站。子卡第 1 步核对该 EN 文档恰有一行等于 `S_U`，网站「页面检查」（变更分支的第 7 步或无变更分支的页面检查）另以 `S_U` 为判定句须为含（家族统一的同卡文档门内容，不计入各子卡 G-03 分子）。`S_U` 只写 EN 文档（网站只有 EN 页；zh-CN 与开发文档随子卡本地提交，`DESC-11` 合入前不公开）。
  - **发布点 `DESC-11` 的文档步骤（`ADR-DESC-04`；即其 AC5、AC6，计入 G-03）：** ① ER-05 评审前（B 组）从 `docs/commands/rev-parse.md` 删除恰等于 `S_U` 的那一行、不做其它文档改动（AC5），该删除随发布提交进入 `main`（AC3 绑定）；② D 组全绿（`release.yml` 全部 job `success` 且三个下载地址 HTTP 200）之后执行第 2–6 步（第 3 步此时必为变更分支：网站含 `S_U` 而仓库文档已删除；不选 `M`；仓库文档以 `/Volumes/Data/gitmono/libra` 中已含发布提交的 `main` 为准，第 2 步前在该工作区核对 `libra merge-base --is-ancestor "$MERGE_SHA" HEAD` 退出 0、`libra status --short --no-branch -- docs/commands/rev-parse.md` 输出为空，且 `grep -c -x -F -- "$S_U" docs/commands/rev-parse.md` 输出 `0`，任一不符即 STOP），第 7 步改为「删除核验」：推送前以 `S_U` 为判定句的「页面检查」须为含（前置），推送后按 ER-10 有界轮询（≤ 5 次、总计 ≤ 30 分钟）直到该检查为不含（AC6），超预算即 `blocked`；D 组未全绿时不执行②，网站保持 `S_U`。`DESC-11` 不执行第 1 步（仓库文档由 AC5 判定；zh-CN、开发文档与 `COMPATIBILITY.md:217` 不变）。
  - **退出码捕获（本节通用）：** 本节所有按退出码判定的命令（`test`、`rg`、`curl`、`libra merge-base --is-ancestor` 等）一律以 `if <命令>; then rc=0; else rc=$?; fi` 的条件形式捕获退出码（模板「Verification 判定口径」），在 `set -e` / `errexit` 下也不会因预期的非 0 退出（如 `rg` 的「不含」= 1）而提前中止；随后按各步写明的取值判定。
  1. **仓库文档逐文件验收：** 对卡内 `Docs and compatibility impact` 列出的每个仓库内文件，记录修改前后 `shasum -a 256 <file>`；以 `rg -n -F '<本卡 flag 或语法关键词>' <file>`（锚点定位用，非判据）定位并人工确认对应章节已描述本卡行为（EN 与 zh-CN 均需覆盖）；`COMPATIBILITY.md` 只原地改写 `:217`（`rev-parse`）或 `:219`（`describe`）既有行（提交前执行）：先 `if base="$(libra show HEAD:COMPATIBILITY.md)"; then rc=0; else rc=$?; fi`，`rc` 非 0 或 `base` 为空即 STOP；再要求 `printf '%s\n' "$base" | wc -l` 与 `wc -l < COMPATIBILITY.md` 相等，且 `if diff <(printf '%s\n' "$base" | sed '<行号>d') <(sed '<行号>d' COMPATIBILITY.md); then rc=0; else rc=$?; fi` 的 `rc` 为 0（基线只存于 shell 变量，进程替换只经管道，不写文件；1 = 其它行被改即 STOP，>1 判执行失败）。缺失任一文件或内容不符即 STOP。
  2. **网站前置（第 3 步写入前完成）：** ① `cd /Volumes/Data/gitmono/libra-backend && test -d .libra && test ! -e .git`（退出 0 即仅含 Libra 元数据）；② `libra status --short --branch` 首行以 `## cf...origin/cf` 开头（按 `AGENTS.md:135` 直接在 `cf` 上工作：不在 `cf` 时 STOP 并报告，不代为切换）；以 `libra status --porcelain -z --untracked-files=all`（NUL 分隔，可处理特殊文件名；未跟踪目录递归展开为文件）记录全部既有无关改动，每条记录状态列、全部路径（重命名/复制记录的新旧两个路径都记录）与指纹（普通文件为「类型 + 权限位（macOS `stat -f %Lp`、Linux `stat -c %a`）+ `shasum -a 256`」；符号链接为「类型 + `readlink` 目标」；工作区中已不存在的路径记为 `absent`；其它类型或无法读取即 STOP），只存于 shell 变量与本卡记录，并保持不动；`libra remote -v` 的 `origin` 指向 `libra-tools/libra-backend`；③ `libra status --short --no-branch -- apps/tanstack-app/content/docs/commands/<cmd>.en.md` 输出为空（目标文件无他人未提交改动）；④ 暂存区为空：`libra status --porcelain` 输出中不存在首列（暂存区状态）为非空格且非 `?` 的行（Libra 提交整个暂存区，他人已暂存的改动会随本卡提交被推送）；⑤ 远端对账：`libra fetch origin` 退出 0 后比较 `libra rev-parse HEAD` 与 `libra rev-parse origin/cf`——相等即通过并在本卡记录该 SHA；不等且 `libra merge-base --is-ancestor HEAD origin/cf` 退出 0（本地仅落后）时执行 `libra pull --ff-only origin cf`（须退出 0）后从 ② 重新核验；不等且该命令退出 1（本地有未推送提交或已分叉）即 STOP 并报告归属，不得把他人提交随本卡推送。任一条不满足或任一命令出现非预期退出码即 STOP 并报告。
     - **续作例外（同一卡在第 3–7 步中断后重入）：** 重入时先执行 ①②，再以本卡已记录的证据识别本卡自己的残留，只放行能逐项对上的下列状态，其余一律按 ③–⑤ STOP：(i) **已写入未暂存**——目标文件当前 SHA-256 等于第 3 步记录的写后值，暂存区为空，`libra rev-parse HEAD` 等于第 2 步记录的 `origin/cf` SHA：从第 4 步继续；(ii) **已暂存未提交**——`libra status --porcelain` 的暂存路径集合恰为本卡网站文件且第二列为空格，文件 SHA-256 等于第 5 步受审值，`HEAD` 同 (i)：从第 6 步的提交继续；(iii) **已提交未推送**——`libra rev-parse HEAD` 等于第 6 步记录的本卡提交 SHA，`libra rev-parse 'HEAD^'` 等于第 2 步记录的 `origin/cf` SHA，且 `libra fetch origin` 后 `libra rev-parse origin/cf` 仍等于该记录值：从第 6 步提交后的核验继续并推送；(iv) **已推送**——`libra ls-remote origin refs/heads/cf` 输出恰一行，其第一列 hash 等于记录的本卡提交 SHA：从第 7 步继续。无法归入上述任一状态或任一核对不符即 STOP 并报告。
  3. **网站同步与路径判定：** 先 `python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --backend-root /Volumes/Data/gitmono/libra-backend --check --only <cmd>`（按退出码捕获）。**变更分支**（退出 1 = 网站与第 1 步已验收的仓库文档有漂移）：写入前先按第 7 步选定本卡新增句 `M` 并确认零命中，再以同一命令的 `--write --only <cmd>` 写入，之后再次 `--check --only <cmd>` 须退出 0；记录网站文件修改前后 SHA-256，且 `libra status --short --no-branch -- apps/tanstack-app/content/docs/commands/<cmd>.en.md` 须非空（为空即 STOP），随后执行第 4–7 步。**无变更分支**（退出 0 = 无漂移）：不选 `M`、不写入；工作区内容即已提交内容，人工确认其已描述本卡行为（未描述即 STOP 并核查网站与仓库文档为何不一致），摘取一句描述本卡行为的纯文本句 `W`（不含 Markdown 标记，也不含会被 HTML 转义的 `'`、`"`、`&`、`<`、`>`），`rg -F -q -- '<W>' <网站文件>` 须退出 0，再执行第 4 步并对线上页面执行第 7 步定义的「页面检查」（判定句为 `W`）：含 `W` 即记录为「网站未变更」证据；不含则按 ER-10 有界轮询（≤ 5 次、总计 ≤ 30 分钟），超预算仍未出现即本卡置 `blocked` 并报告，不得标 `complete`；无变更分支不执行第 5、6 步与第 7 步的 `M` 判定。其它退出码即 STOP。
  4. **前端验证：** 在 `/Volumes/Data/gitmono/libra-backend` 运行 `pnpm typecheck`（根 `package.json:15` → `apps/tanstack-app` 的 `pnpm gen:docs && tsc --noEmit`）与 `pnpm build`（根 `package.json:12` → `CF_DEPLOY=1 vite build`），均退出 0（无变更分支亦执行）。`pnpm typecheck` 经 `gen:docs`（`apps/tanstack-app/package.json:12-13`）重写被忽略的生成文件 `apps/tanstack-app/src/generated/docs-manifest.ts` 与 `docs-search.ts`，`pnpm build` 重写被忽略的 `apps/tanstack-app/dist/`。这三处是网站仓库约定只由生成器 / 构建写入的派生产物，已列入每张卡的 `Implementation write set`，并按 `DEP-DESC-DOCS` 只在持有协调者对网站交付的串行窗口时执行；执行前逐个检查两个生成文件：不存在时记录 `absent`；存在时首行不是生成器标注 `AUTO-GENERATED by scripts/gen-docs-manifest.ts — do not edit` 即 STOP 并报告，是则记录其 SHA-256；执行后两者都须存在且首行为该标注（否则判第 4 步失败）。不得手工编辑、暂存或提交这三处。
  5. **独立评审：** 对本卡网站改动（`libra diff -- <网站文件>`）取得独立 Claude Code 字面 `VERDICT: PASS`，记录受审文件 SHA-256；评审后受审文件 SHA-256 变化即须重审。该网站交付门独立于计划文本的 Claude + Codex 双审，二者互不替代。
  6. **提交与推送：** `libra add <本卡网站文件精确路径>` 后，`libra status --porcelain` 中首列非空格且非 `?` 的路径集合须恰为该文件（否则 STOP）；`libra commit -s -m "docs(<cmd>): ..."` 后，`libra show --name-only --format=format: HEAD` 列出的非空行集合须恰为该文件（否则 STOP，不得推送）；按 ER-07 核验同一提交的 `gpgsig` 与 `Signed-off-by`；记录 `libra rev-parse HEAD` 与 `libra rev-parse 'HEAD^{tree}'`，并以 `libra show HEAD:<path> | shasum -a 256` 确认提交内容等于第 5 步受审 SHA-256；`libra push origin cf` 后 `libra ls-remote origin refs/heads/cf` 须恰输出一行，其 hash 等于 `libra rev-parse HEAD`；推送被拒（远端已前移）即 STOP 并报告，不得强推。推送后以第 2 步同一命令与同一指纹规则重新枚举：除本卡网站文件外的条目集合（状态列、路径与内容指纹）须与第 2 步记录逐项相等，任一缺失、新增或变化即 STOP 并报告（证明无关改动未被覆盖、暂存或提交）。
  7. **部署核验（仅变更分支；「页面检查」两分支通用）：** 「页面检查」是本计划核对线上页面的唯一方法，页面内容只保存在 shell 变量中，不写任何文件（v2.12）：先 `if page="$(curl -fsSL 'https://libra.tools/en/docs/commands/<cmd>')"; then rc=0; else rc=$?; fi`，`rc` 非 0 即 STOP（不得视为「不含」）；再 `if rg -F -q -- '<判定句>' <<<"$page"; then hit=0; else hit=$?; fi`，`hit` 为 0 = 含、1 = 不含、>1 判执行失败；每次检查的 `rc`、`rg` 退出码与 UTC 时间写入本卡记录。变更分支在第 3 步写入前为网站文件选定本卡新增的一句描述本卡行为的唯一纯文本句 `M`（取自第 1 步本卡新增的仓库文档文本，约束同 `W`），并确认零命中：`rg -F -q -- '<M>' <网站文件>` 退出 1（>1 判执行失败），且「页面检查」结果为不含；推送后按 ER-10 有界轮询（≤ 5 次、总计 ≤ 30 分钟）执行「页面检查」直到含 `M`，记录命中时间与第 6 步的 `cf` 提交 SHA。若 `M` 在推送前已出现或超预算仍未出现，本卡置 `blocked`，不得标 `complete`。

## 实施顺序

因 `DESC-01..08`、`DESC-01a`、`DESC-08a` 共享 `src/command/describe.rs` 与命令文档，`DESC-09` 为 `DESC-05`/`DESC-04` 往返解析前置，全链按 G-10 串行执行：

- `DESC-01 -> DESC-01a`
- `DESC-01a -> DESC-02`
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
- `DESC-10k -> DESC-11`
- `DESC-11 -> DESC-05`
- `DESC-05 -> DESC-04`
- `DESC-04 -> DESC-06`
- `DESC-06 -> DESC-07`
- `DESC-07 -> DESC-08`
- `DESC-08 -> DESC-08a`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-DESC-DOCS | incoming | 相邻仓库文档分支与网站交付串行窗口 | `/Volumes/Data/gitmono/libra-backend`（唯一网站工作区，`cf` 生产分支；依据：用户 2026-11-18 11:55:09 UTC 确认。`AGENTS.md:135` 所写 `/Volumes/Sea/libra-backend` 为已知文档滞后，本计划不修改该文件） | 当前执行者；串行窗口由协调者分配 | 每张卡写网站前持有协调者对该网站工作区的串行窗口（同批计划共用，与 `DEP-GII-03` 同一窗口），并按「文档与网站验收命令」第 2 步核验单一 `.libra` VCS、`## cf...origin/cf`、`origin` 指向 `libra-tools/libra-backend`、暂存区为空与远端对账；按第 3–7 步同步、构建、取得独立 Claude Code `PASS`、精确提交推送并核验部署 | `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/{describe,rev-parse}.en.md`（2026-11-18 核对存在；`libra status --short --branch` 为 `## cf...origin/cf`；仅含 `.libra`） | 路径缺失、双 VCS、非 `cf`、目标文件有他人改动、暂存区非空或本地分叉即 STOP 并向协调者报告，禁止另建分支、worktree、独立克隆或改用其它 checkout；未完成前不得标 `complete` |
| DEP-DESC-COMPAT | incoming | 共享文件写集 | `COMPATIBILITY.md`（本计划只原地修改 `:217` `rev-parse` 与 `:219` `describe`） | 协调者 | 每张卡开工编辑前确认同批计划无在跑卡正在修改 `COMPATIBILITY.md`，取得串行编辑窗口后才修改、验收并提交，提交后释放 | 本计划依赖表；`global-init-infra` 的 `DEP-GII-03` 同类登记 | 未释放则等待，禁止并发改同一文件（G-10） |
| DEP-DESC-UTIL | incoming | 跨计划写集协调 | 「与其它计划的关系」所列 `branch`、`clone`、`merge-pull`、`global-init-infra`、`plumbing-misc` 各卡对 `src/utils/util.rs`、`tests/command/rev_parse_test.rs`、`docs/commands/{,zh-CN/}rev-parse.md`、`docs/development/commands/rev-parse.md`、网站 `rev-parse.en.md` 的写集 | 协调者 | `DESC-09`、`DESC-09a..09c`、`DESC-11`、`DESC-10`、`DESC-10a..10k` 各自开工前核验上述计划无在跑卡持有这些文件（以开工当日各计划写集与 `plan-status.md` 为准），并刷新 `src/utils/util.rs:2319-2468` 行号 | `docs/development/plan/plan-20261117-{branch,clone,merge-pull,global-init-infra,plumbing-misc}.md`（2026-11-18 核对） | 任一计划正在修改共享文件时暂停对应 `DESC-*` 卡直至写集释放，再按 ER-02 刷新锚点后开工 |
| DEP-DESC-541 | incoming | 跨计划/上游能力前置 | `issues/541.md`（`#541`：`update-ref` 支持 `refs/tags/*`） | `update-ref` 计划负责人 | `libra update-ref refs/tags/Q <oid>` 与 `libra update-ref -d refs/tags/A` 可在上游 `t6120-describe.sh:123-132` 正常执行 | `docs/development/plan/issues/541.md`、`src/command/update_ref.rs:6-10,147-150` | 门按卡分开：`DESC-05` 在 `#541` 未合入或 `TC-1045` 未 `PASS` 前保持 `blocked`、`Acceptance` 为空，不得进入 C/D；`DESC-04` 在 `DESC-05` 未 `done / complete`、`#541` 未合入或 `TC-1043` 未 `PASS` 前同样 `blocked`；`DESC-06` 依赖 `DESC-04`。仓内 focused 测试可先写但不得发布。14 天未合入由协调者升级重排 `#541` |
| DEP-DESC-TC | incoming | 外部上游与 TC 验收夹具 | `../libra-testcases/cases/tc{1031,1034,1035,1043,1045,1046,1052,1054}-*`（`t/t6120-describe.sh` @ `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`，harness 固定于 `cases/lib/tc-harness.sh:65`） | 协调者 / 兼容性测试运行者 | 每次运行先在当前 shell 执行 `export TC_RUN_ID="desc-<card>-$(date -u +%Y%m%dT%H%M%SZ)-$$" TC_TMP_ROOT=/Volumes/Data/tmp/libra-testcases`（唯一运行标识，且父 shell 与脚本使用同一缓存根；同一 `TC_RUN_ID` 内已有 `exit` 文件的上游文件不会重跑，`tc-harness.sh:290-295`；`TC_TMP_ROOT` 与 harness 在 `/Volumes/Data/tmp` 存在时的默认值相同，`tc-harness.sh:67-73`）；执行本卡 `tc*.sh`，要求脚本退出码 0 **且** stdout 含精确行 `TC-<id> PASS`（`tc-harness.sh:565-568`；`SKIP`/`XFAIL` 也会退出 0，故不能只看退出码）；把 `TC_RUN_ID`、`shasum -a 256 "$PWD/target/debug/libra"`、`libra rev-parse HEAD`、脚本退出码、该 PASS 行与 `$TC_TMP_ROOT/runs/$TC_RUN_ID/libra/t6120-describe/exit`（`tc-harness.sh:175-178, 233, 276`） 的值写入本卡记录（ER-11；harness 的运行目录是其自有缓存，不是本计划证据文件） | `../libra-testcases/cases/tc1031-describe-contains/tc1031-describe-contains.sh` 等 8 个脚本（2026-11-18 核对存在） | `../libra-testcases` 缺失、harness 上游提交不符或脚本未输出 `TC-<id> PASS`（`FAIL`/`XPASS`/`SKIP`/`XFAIL`）时，相关卡置 `Lifecycle=blocked` 且 `Acceptance` 为空：不得进入 C/D、不得标 `complete`/`done`，也不得关闭对应 Issue |

### 发布分组与并发窗口

除下列家族外，其余卡均为 `independent` 独立发布切片。

**REL-DESC-09（G-08 家族：describe 后缀解析，根命名空间与六种前缀命名空间）：**
- **成员与顺序：** `DESC-09`（1/16）→ `DESC-09b`（2/16）→ `DESC-09c`（3/16）→ `DESC-09a`（4/16）→ `DESC-10`（5/16）→ `DESC-10a`（6/16）→ `DESC-10b`（7/16）→ `DESC-10c`（8/16）→ `DESC-10d`（9/16）→ `DESC-10e`（10/16）→ `DESC-10f`（11/16）→ `DESC-10g`（12/16）→ `DESC-10h`（13/16）→ `DESC-10i`（14/16）→ `DESC-10j`（15/16）→ `DESC-10k`（16/16），唯一发布点 `DESC-11`（`Task type=release`）。
- **不可分割理由：** 根命名空间 describe 后缀的语法、commit 解析、缺失/歧义消歧、tag 与 blob/tree 放行共享同一 helper；任一子集单独发布都必须人为限制对象类型或消歧并为每个被限制项增设边界判据，R48–R50 评审已证明单张可发布卡的独立判据因此超过 G-03 上限（R50 拆出的未接线纯函数卡又被判违反 G-02）。前缀命名空间同理：R61 评审证明，若 `DESC-10..10k` 各自独立发布，每个中间版本都须对「缩写与全长 OID × tree/blob/附注 tag」及「尚未接入的其它前缀」逐项设临时边界判据，单卡判据超过 G-03 上限。故十六张子卡各自评审、各自通过 A 组与 fmt/clippy、各自本地签名+DCO 提交，家族内的类型与前缀放行顺序只是实现顺序（中间态只存在于本地子卡提交、不对外发布，故不设临时边界判据），由 `DESC-11` 一次对外发布完整、与 Git 一致的根与前缀命名空间行为。
- **窗口规则：** 自 `DESC-09` 开工至 `DESC-11` 的 AC6（D 组全绿后网站删除 `S_U` 的核验）完成为家族窗口；`DESC-09` 开工时（任何实现工作之前）先记录家族基线：网站页 SHA-256 与 `origin/cf` SHA；窗口内子卡不 bump、不构建发布、不推送代码提交；子卡按 ER-06a 在本卡内交付网站页（第 1–7 步），页面含「尚未发布」说明句 `S_U`（见「文档与网站验收命令」家族条目、`ADR-DESC-04` 与「风险登记」）；其它卡不得修改 `src/utils/util.rs`、`tests/command/rev_parse_test.rs` 与 `rev-parse` 文档（与 `DEP-DESC-UTIL` 一致）；子卡提交若被 rebase 改写，逐个对新 SHA 重验 ER-07 的签名与 DCO，无法证明即 STOP。
- **失败恢复（按所处状态，互斥）：** ① 家族提交与 `DESC-11` 的版本提交均未推送（含家族中途任一子卡失败）：按本计划已记录的实际进度只处理已存在的部分——若 `DESC-11` 已产生版本面或 `docs/commands/rev-parse.md`（删除 `S_U`）改动，未提交时以 `libra restore --staged --worktree -- <版本面集合> Cargo.lock docs/commands/rev-parse.md` 恢复（只含本卡改动时；混有他人改动即 STOP），已本地提交时 `libra revert <DESC-11 提交>`，并核对版本面与 `compat_version_surface_sync` 一致回到 bump 前；失败子卡的未提交改动按「字段全局默认」的未提交恢复规则处理；已创建的子卡代码提交按家族顺序的逆序 `DESC-10k` → `DESC-10j` → `DESC-10i` → `DESC-10h` → `DESC-10g` → `DESC-10f` → `DESC-10e` → `DESC-10d` → `DESC-10c` → `DESC-10b` → `DESC-10a` → `DESC-10` → `DESC-09a` → `DESC-09c` → `DESC-09b` → `DESC-09` 中实际存在者逐个 `libra revert`；网站按各子卡的实际网站进度处理：「已写入未暂存」须暂存区为空且工作区文件 SHA-256 等于该子卡第 3 步记录的写后值；「已暂存未提交」须 `libra status --porcelain` 的暂存路径集合恰为该网站文件、其第二列为空格（暂存内容与工作区一致）且工作区 SHA-256 等于写后值——任一不符即 STOP（属他人改动）；核对通过后以 `libra restore --staged --worktree -- apps/tanstack-app/content/docs/commands/rev-parse.en.md` 恢复；已提交未推送的本卡网站提交（`HEAD` 为该提交且 `HEAD^` 为其第 2 步记录的 `origin/cf` SHA）以 `libra revert` 撤回后与之一并推送（净内容不变，使本地与 `origin/cf` 一致）；已推送的网站提交逐一以恢复改动前内容的新 `cf` 提交补偿；最后核验 `libra log` 中家族代码提交均已被撤回、网站页 SHA-256 等于家族基线（`DESC-09` 开工时、任何实现工作之前无条件记录的 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` SHA-256 与当时 `origin/cf` SHA）；② `DESC-11` 的 PR 已推送但未合入：先 `gh pr close` 关闭该 PR 并删除其远端分支，核实 `origin/main` 未含家族提交（`libra merge-base --is-ancestor <DESC-09 提交> origin/main` 退出 1）后按 ① 处理；③ 已合入 `main`、`v${VERSION}` tag 与 GitHub Release 均未创建：不得回退 `main`，按 ER-10 新建 `FIX-*` 前滚修复，再在包含修复的新 `MERGE_SHA` 上重新执行 `DESC-11` 的 C 组 bump 之后各步（版本号按 ER-08 以当时 `main` 为基线重新确定）；④ `v${VERSION}` tag 或 GitHub Release 已创建（无论 D 组结果）：该版本不可撤回，按 `DESC-11` 的 `immutable-release` 以新 patch 前滚修复并重新取得新版本的 C/D 证据，必要时指引降级到 `PREV_VER`。③④ 两种状态下网站保持 `S_U`（`DESC-11` 的 AC6 只在含家族的版本 D 组全绿之后执行）。
- **验收状态：** 子卡 A 组与 fmt/clippy 通过即 `locally-accepted`；`DESC-11` 的 C 组（其收口测试门被测树含全部子卡最终变更）通过后子卡转 `remote-pending`，`DESC-11` 的 D 组全绿且 AC6 完成后与发布点一同 `complete`（ER-04；此前保持 `remote-pending`）。

**并发声明:** 全串行（`DESC-01..08`、`DESC-01a`、`DESC-08a` 共享 `src/command/describe.rs`；`DESC-09`、`DESC-09a..09c`、`DESC-10`、`DESC-10a..10k` 共享 `src/utils/util.rs` 与 `tests/command/rev_parse_test.rs`；`DESC-11` 与各 `rev-parse` 卡共享网站 `rev-parse.en.md`；全部卡共享 `COMPATIBILITY.md` 与网站工作区）。

**发布者:** 由协调者指定的单一发布者串行执行 C 组 bump/构建/提交/推送/`gh release create` 并跟踪 D 组结果（ER-12）。

**发布窗口顺序:** `DESC-01` → `DESC-01a` → `DESC-02` → `DESC-03` → `DESC-11`（家族 `REL-DESC-09`：`DESC-09`、`DESC-09b`、`DESC-09c`、`DESC-09a`、`DESC-10`、`DESC-10a`、`DESC-10b`、`DESC-10c`、`DESC-10d`、`DESC-10e`、`DESC-10f`、`DESC-10g`、`DESC-10h`、`DESC-10i`、`DESC-10j`、`DESC-10k` 依次本地签名提交后由其统一发布） → `DESC-05` → `DESC-04` → `DESC-06` → `DESC-07` → `DESC-08` → `DESC-08a`。

### Phase 0: 基线核对与文档分支确认

**目标:** 确认工作区干净、上游基线与 `/Volumes/Data/gitmono/libra-backend` `cf` 分支状态。

**进入条件:**
- 计划文本经 Claude（Subagent）与 Codex 对同一字节双审均返回 `VERDICT: PASS`（用户 2026-11-18 指令）。
- [plan-status.md](plan-status.md) 已登记 `plan-20261117-describe.md` 状态行。

**退出条件:**
- `ER-01` 与 `DEP-DESC-DOCS` 核验记录就绪。

### Phase 1: `describe` 过滤、`rev-parse` describe 后缀与 Misnamed Tag / 缩写修复（`DESC-01`、`DESC-01a`、`DESC-02`、`DESC-03`、`DESC-09`、`DESC-09b`、`DESC-09c`、`DESC-09a`、`DESC-10`、`DESC-10a..10k`、`DESC-11`、`DESC-05`、`DESC-04`、`DESC-06`）

**目标:** 依次修复 `--contains ^0`（主路径与 `--exact-match` 分支）、`--no-match`、`--all` + `--match`、`rev-parse` describe 后缀解析（根命名空间与六种命名空间）、misnamed annotated tag、`--no-long` 与 `--no-abbrev`。

**进入条件:**
- Phase 0 退出条件满足。

**退出条件:**
- `DESC-01`、`DESC-01a`、`DESC-02`、`DESC-03`、`DESC-09`、`DESC-09b`、`DESC-09c`、`DESC-09a`、`DESC-10`、`DESC-10a..10k`、`DESC-11` 全部 `done / complete`（家族子卡在 `DESC-11` 的 C/D 全绿后才可 `complete`）。`DESC-05` 在 `#541` 合入且 `TC-1045` `PASS` 后 `done / complete`。`DESC-04` 在 `DESC-05` `done / complete` 且 `#541` 合入且 `TC-1043` `PASS` 后 `done / complete`。`DESC-06` 随后 `done / complete`。未满足各自 TC 前对应卡 `Lifecycle=blocked`、`Acceptance` 为空。

### Phase 2: `--dirty` 适用范围与 `--broken` 支持（`DESC-07`、`DESC-08`、`DESC-08a`）

**目标:** 对齐 `--dirty` 的适用范围并实现 `--broken[=<mark>]`，随后执行计划级全量收口门。

**进入条件:**
- `DESC-06` 达到 `done / complete`。

**退出条件:**
- `DESC-07`、`DESC-08`、`DESC-08a` 全部 `done / complete`，且全量收口门（fmt + clippy + `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`）全绿。

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

- **被取消 run 的处理:** 被 `cancel-superseded-main.yml` 取消的 `base.yml`/`codeql.yml` run 不计 PASS，须取同一 `MERGE_SHA` 的后续成功 run；无则保持 `remote-pending` 或 STOP。

**默认覆盖：** 无。

**规则 waiver（`EX-*`）：** 无。

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| DESC-01 | implementation | --contains 主路径附注 tag 精确匹配 ^0 | immutable-release：前滚新 patch / 降级 PREV_VER | yes | yes | 8/8 | 2/8 | 1/1 | M | DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DEP-DESC-COMPAT/DEP-DESC-DOCS 窗口 | independent | N/A | N/A |
| DESC-01a | implementation | --contains --exact-match 分支附注 tag ^0 | immutable-release：前滚新 patch / 降级 PREV_VER | yes | yes | 6/8 | 1/8 | 1/1 | M | DESC-01, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-01 | independent | DESC-01 | N/A |
| DESC-02 | implementation | --no-match 重置 --match 过滤 | immutable-release：前滚新 patch / 降级 PREV_VER | yes | yes | 7/8 | 3/8 | 1/1 | M | DESC-01a, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-01a | independent | N/A | N/A |
| DESC-03 | implementation | --all 候选受 --match/--exclude 过滤 | immutable-release：前滚新 patch / 降级 PREV_VER | yes | yes | 8/8 | 2/8 | 1/1 | M | DESC-02, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-02 | independent | N/A | N/A |
| DESC-09 | implementation | 根命名空间 describe 后缀语法与 commit 解析 | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 8/8 | 1/8 | 1/1 | M | DESC-03, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-03 | family child | N/A | N/A |
| DESC-09b | implementation | describe 后缀 OID 无效、缺失与歧义语义 | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 7/8 | 1/8 | 1/1 | M | DESC-09, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-09 | family child | DESC-09 | N/A |
| DESC-09c | implementation | 根命名空间后缀放行附注 tag 对象 | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 2/8 | 1/8 | 1/1 | M | DESC-09b, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-09b | family child | DESC-09 | N/A |
| DESC-09a | implementation | 根命名空间后缀放行 blob 与 tree | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 4/8 | 1/8 | 1/1 | M | DESC-09c, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-09c | family child | DESC-09 | N/A |
| DESC-10 | implementation | tags/ 命名空间后缀解析 commit | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 7/8 | 1/8 | 1/1 | M | DESC-09a, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-09a | family child | DESC-09 | N/A |
| DESC-10a | implementation | heads/ 命名空间后缀解析 commit | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 5/8 | 1/8 | 1/1 | M | DESC-10, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10 | family child | DESC-09 | N/A |
| DESC-10b | implementation | remotes/ 命名空间后缀解析 commit | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 5/8 | 1/8 | 1/1 | M | DESC-10a, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10a | family child | DESC-09 | N/A |
| DESC-10c | implementation | refs/tags/ 命名空间后缀解析 commit | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 5/8 | 1/8 | 1/1 | M | DESC-10b, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10b | family child | DESC-09 | N/A |
| DESC-10d | implementation | refs/heads/ 命名空间后缀解析 commit | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 5/8 | 1/8 | 1/1 | M | DESC-10c, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10c | family child | DESC-09 | N/A |
| DESC-10e | implementation | refs/remotes/ 命名空间后缀解析 commit | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 5/8 | 1/8 | 1/1 | M | DESC-10d, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10d | family child | DESC-09 | N/A |
| DESC-10f | implementation | tags/ 命名空间后缀放行非提交对象 | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 7/8 | 1/8 | 1/1 | M | DESC-10e, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10e | family child | DESC-09 | N/A |
| DESC-10g | implementation | heads/ 命名空间后缀放行非提交对象 | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 7/8 | 1/8 | 1/1 | M | DESC-10f, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10f | family child | DESC-09 | N/A |
| DESC-10h | implementation | remotes/ 命名空间后缀放行非提交对象 | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 7/8 | 1/8 | 1/1 | M | DESC-10g, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10g | family child | DESC-09 | N/A |
| DESC-10i | implementation | refs/tags/ 命名空间后缀放行非提交对象 | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 7/8 | 1/8 | 1/1 | M | DESC-10h, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10h | family child | DESC-09 | N/A |
| DESC-10j | implementation | refs/heads/ 命名空间后缀放行非提交对象 | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 7/8 | 1/8 | 1/1 | M | DESC-10i, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10i | family child | DESC-09 | N/A |
| DESC-10k | implementation | refs/remotes/ 命名空间后缀放行非提交对象 | compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站 | yes | yes | 7/8 | 1/8 | 1/1 | M | DESC-10j, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-10j | family child | DESC-09 | N/A |
| DESC-11 | release | REL-DESC-09 聚合发布 | immutable-release：前滚新 patch / 降级 PREV_VER | yes | yes | 6/12 | 6/12 | 0/0 | S | DESC-10k, DEP-DESC-UTIL, DEP-DESC-DOCS | 序列化于 DESC-10k | family release point | N/A | N/A |
| DESC-05 | implementation | misnamed tag 内部名与强制后缀 | immutable-release：前滚新 patch / 降级 PREV_VER | yes | yes | 8/8 | 2/8 | 1/2 | M | DESC-11, DEP-DESC-541, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-11 | independent | N/A | N/A |
| DESC-04 | implementation | --no-long 与 --long 的 last-wins | immutable-release：前滚新 patch / 降级 PREV_VER | yes | yes | 8/8 | 3/8 | 1/1 | M | DESC-05, DEP-DESC-541, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-05 | independent | N/A | N/A |
| DESC-06 | implementation | --no-abbrev 等价 --abbrev=0 的 last-wins | immutable-release：前滚新 patch / 降级 PREV_VER | yes | yes | 8/8 | 3/8 | 1/2 | M | DESC-04, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-04 | independent | N/A | N/A |
| DESC-07 | implementation | --dirty 的适用范围 | immutable-release：前滚新 patch / 降级 PREV_VER | yes | yes | 8/8 | 3/8 | 1/2 | M | DESC-06, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-06 | independent | N/A | N/A |
| DESC-08 | implementation | --broken 的脏标记与适用范围 | immutable-release：前滚新 patch / 降级 PREV_VER | yes | yes | 8/8 | 3/8 | 1/2 | M | DESC-07, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC | 序列化于 DESC-07 | independent | N/A | N/A |
| DESC-08a | implementation | --broken 脏检查失败时追加标记 | immutable-release：前滚新 patch / 降级 PREV_VER | yes | yes | 5/8 | 2/8 | 1/2 | M | DESC-08, DEP-DESC-DOCS, DEP-DESC-COMPAT | 序列化于 DESC-08 | independent | DESC-08 | N/A |

---

### Task DESC-01: 修复 `libra describe --contains` 在精确匹配附注 tag 时缺少 `^0` 后缀（#795）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`0.30.214`；targeted 3/3、共享 describe 组 34/34、版本守卫 2/2、`TC-1031 PASS`；Codex R3 `VERDICT: PASS`；EN/zh/开发/COMPATIBILITY/网站已同步；远端 CI 未等待，按用户 2026-11-20 指令 local C/D） | **Priority / Selection:** P1；Git 核心兼容缺陷 | **Issue:** [#795](https://github.com/libra-tools/libra/issues/795)

**Description:** 修复 `run_describe_contains` 主循环：目标提交直接被**附注 tag**指向时输出 `<tag>^0`（轻量 tag 仍输出 `<tag>`，对齐 Git `name-rev --peel-tag`）。「直接指向」以种子状态判定：Dijkstra 权重 `weight == 0`（只有 tag 种子权重为 0；首父边 `+1`、非首父边 `+MERGE_COST`，`src/command/describe.rs:437, 456, 515-518`），**不得**用 `generation == 0`——非首父边把新状态置为 `("<display>^<n>", 0)`（`:518`），以它判定会把合并提交的第二父 `v2.0^2` 误判为精确匹配。`data.exact_match` 改为按同一种子条件计算（现为 `display == tag`，`:479-482`，追加 `^0` 后会误判为 `false`）；同步更新既有 `describe_test.rs:555` 的旧断言。`--exact-match`/`--candidates 0` 的提前返回分支（`:411-427`）本卡不改，按 G-09 拆出 `DESC-01a`：本卡期间该分支仍输出 `<tag>`，既有用例 `test_describe_contains_exact_match_requires_direct_tag`（`:687` 断言 `v1.0`）在共享回归组中保持通过，作为本卡的未改动边界。

**Out of scope:** `--contains --exact-match`/`--candidates 0` 分支的 `^0`（`DESC-01a`）；`--contains` 忽略 `--dirty`（由 `DESC-07` 承接）；`--all` 下的 `--match` 过滤（由 `DESC-03` 承接）；嵌套 tag 多层剥皮（`DEFER-DESC-01`）；`--contains --exact-match` 与 Git 的既有差异（`DEFER-DESC-04`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 主循环 `generation == 0` 时 `display = base.clone()`，未对附注 tag 追加 `^0`；`exact_match = display == tag` | `src/command/describe.rs:464-496`（`:479-482`） |
| 非首父边新状态为 `(format!("{display}^{}", i + 1), 0)`，`generation` 归零；tag 种子权重为 0 | `src/command/describe.rs:437, 456, 515-518` |
| 既有用例在附注 tag 精确命中时断言 `"v1.0"`；合并用例只用轻量 tag `v2.0` 断言 `v2.0^2` | `tests/command/describe_test.rs:555`、`:756-815` |
| pinned Git 实测：附注 tag `v2.0` 指向合并提交时，`git describe --contains <第二父>` 输出 `v2.0^2`、`--contains HEAD` 输出 `v2.0^0` | 本计划 R61 以 pinned 构建（`git version 2.56.0.116.g6de20f6092`）实测 |

**Acceptance criteria:**

- [x] AC1：`HEAD` 被附注 tag 直接指向时，无参数 `libra describe --contains`（默认 `HEAD`，`#795` 上游用例）的 stdout 为 `<tag>^0`。
- [x] AC2：显式 `<commit>` 被附注 tag 直接指向时，`libra describe --contains <commit>` 的 stdout 为 `<tag>^0`。
- [x] AC3：目标被轻量 tag 直接指向时，`libra describe --contains <commit>` 的 stdout 为 `<tag>`（不加 `^0`）。
- [x] AC4：附注 tag 精确匹配时 `libra describe --contains --json` 的 `data.result` 为 `<tag>^0`。
- [x] AC5：同一命令的 `data.tag` 为 `<tag>`（不含 `^0`）。
- [x] AC6：同一命令的 `data.exact_match` 为 `true`。
- [x] AC7：附注 tag 指向合并提交时，`libra describe --contains <合并提交的第二父>` 的 stdout 为 `<tag>^2`（不加 `^0`）。
- [x] AC8：同一目标的 `libra describe --contains --json <合并提交的第二父>` 的 `data.exact_match` 为 `false`。
- [x] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219` 与网站 `describe.en.md`（说明 `--exact-match`/`--candidates 0` 下的 `^0` 尚未支持），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（夹具 A：`create_committed_repo_via_cli()` 后新增提交 `C1` 并 `libra tag -m v1.0 v1.0`（附注，指向 `C1`），再新增提交 `C2` 并 `libra tag lw`（轻量，指向 `C2`），最后 `libra switch --detach <C1>` 使 `HEAD` = `C1`。夹具 B（独立仓库）：`create_committed_repo_via_cli()` 后以 `libra branch --show-current` 记录当前分支名 `<main>`，`libra branch side`；新增提交 `M1`；`libra switch side` 后新增提交 `S1`；`libra switch <main>`，`libra merge side`（前置断言：`libra rev-parse HEAD^2` 等于 `S1`，即生成了合并提交），再 `libra tag -m v2.0 v2.0`（附注，指向合并提交）。「新增提交 `X`」按「共享测试夹具」的约定；所有命令须退出 0（执行条件））

- AC1 → `(new)` `command::describe_test::contains_exact_tag_appends_caret_zero`（夹具 A）：`libra describe --contains` 的 stdout 去尾换行后恰为 `v1.0^0`。
- AC2 → 修改既有 `command::describe_test::test_describe_contains_names_relative_to_descendant_tag`：其 `:555` 对附注 tag `v1.0` 所在提交的 `libra describe --contains <c2>` 断言由 `v1.0` 改为 `v1.0^0`。
- AC3 → `(new)` 同 AC1 用例：`libra describe --contains <C2>` 的 stdout 恰为 `lw`（把 `^0` 加到所有 tag 的错误实现输出 `lw^0`）。
- AC4、AC5、AC6 → `(new)` 同 AC1 用例：`libra describe --contains --json` 的 `data.result == "v1.0^0"`、`data.tag == "v1.0"`、`data.exact_match == true`（各一次比较）。
- AC7、AC8 → `(new)` `command::describe_test::contains_merge_parent_not_exact`（夹具 B）：`libra describe --contains <S1>` 的 stdout 恰为 `v2.0^2`；`libra describe --contains --json <S1>` 的 `data.exact_match == false`（各一次比较；以 `generation == 0` 判定的错误实现分别输出 `v2.0^2^0` 与 `true`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `describe`）。

**Verification:**

- [x] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::describe_test::contains_exact_tag_appends_caret_zero) or test(=command::describe_test::contains_merge_parent_not_exact) or test(=command::describe_test::test_describe_contains_names_relative_to_descendant_tag)'`（前两条 `(new)`，第三条为本卡修改的既有用例；`cargo nextest list` 须恰好列出这 3 条）
- [x] 非 nextest 门（TC-1031 / `t6120#27`）：`export TC_RUN_ID="desc-01-$(date -u +%Y%m%dT%H%M%SZ)-$$" TC_TMP_ROOT=/Volumes/Data/tmp/libra-testcases && cargo build --bin libra && LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1031-describe-contains/tc1031-describe-contains.sh`（判据与记录项按 `DEP-DESC-TC`）
- [x] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::describe_test::/) or test(/^command::describe_long_test::/)'`

**Full-suite trigger:** `T-1：本卡 patch + 1 修改 install.sh / install.ps1 版本面（C 组第 ④ 步在 bump 后跑双环境完整 nextest）` | **Dependencies:** `DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC`

**Implementation write set:** `src/command/describe.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md` 与 `COMPATIBILITY.md`（`:219` `describe` 行原地更新，不改行数），说明 `--contains` 对附注 tag 精确匹配输出 `<tag>^0`、轻量 tag 与经合并父得到的相对名不加，以及 `--exact-match`/`--candidates 0` 下尚未追加 `^0`；网站页由 `python3 scripts/sync-libra-command-docs.py --write --only describe` 生成（命令全文见「文档与网站验收命令」第 3 步）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M`（修改公开 CLI 输出） | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--contains 主路径附注 tag 精确匹配 ^0; recovery=immutable-release：前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DEP-DESC-COMPAT/DEP-DESC-DOCS 窗口; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-01a: `--contains --exact-match`/`--candidates 0` 精确匹配附注 tag 时追加 `^0`（#795）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`run_describe_contains` 的 `exact_match_only` 分支按 `TagInfo::is_annotated` 渲染——附注 tag 直接命中输出 `<tag>^0`、轻量 tag 仍为 `<tag>`，与 `--contains` 主循环一致；`data.tag` 保持纯 tag 名、`data.exact_match` 为 `true`。既有断言 `describe_test.rs` 中 `--contains --exact-match`/`--candidates 0` 的期望改为 `v1.0^0`；新增 `test_describe_contains_exact_match_renders_tag_peel` 覆盖 AC1–AC6。文档同步：`docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`（两处）、`docs/development/commands/describe.md`（两处）与 `COMPATIBILITY.md:219` 移除「仍输出裸标签」的说明；`describe_test` **32/32**；版本面 0.30.258 → 0.30.259。按用户 2026-11-20 指令在 `main` 直接提交推送。） | **Priority / Selection:** P1；Git 核心兼容缺陷 | **Issue:** [#795](https://github.com/libra-tools/libra/issues/795)

**Description:** 自 `DESC-01` 拆出（G-09）。`run_describe_contains` 的 `exact_match_only` 提前返回分支（`--exact-match` 或 `--candidates 0`，`src/command/describe.rs:207, 411-427`）在目标被附注 tag 直接指向时输出 `<tag>^0`（轻量 tag 仍为 `<tag>`），与 `DESC-01` 的主循环一致；`data.tag` 保持纯 tag 名、`data.exact_match` 保持 `true`（`ADR-DESC-02`）；同步更新既有 `describe_test.rs:687` 的旧断言。

**Out of scope:** `--contains` 主循环（`DESC-01` 已交付）；`--contains --exact-match` 与 Git 的既有差异（`DEFER-DESC-04`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--candidates 0` 与 `--exact-match` 合并为同一 `exact_match` 标志；`exact_match_only` 分支直接返回 `info.name.clone()` | `src/command/describe.rs:207`、`:411-427` |
| 既有用例对两种拼写在附注 tag 精确命中时断言 `"v1.0"` | `tests/command/describe_test.rs:676-688` |

**Acceptance criteria:**

- [ ] AC1：附注 tag 直接指向 `<commit>` 时，`libra describe --contains --exact-match <commit>` 的 stdout 为 `<tag>^0`。
- [ ] AC2：同一目标的 `libra describe --contains --candidates 0 <commit>` 的 stdout 为 `<tag>^0`。
- [ ] AC3：轻量 tag 直接指向 `<commit>` 时，`libra describe --contains --exact-match <commit>` 的 stdout 为 `<tag>`。
- [ ] AC4：附注 tag 精确匹配时 `libra describe --contains --exact-match --json <commit>` 的 `data.result` 为 `<tag>^0`。
- [ ] AC5：同一命令的 `data.tag` 为 `<tag>`（不含 `^0`）。
- [ ] AC6：同一命令的 `data.exact_match` 为 `true`。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219` 与网站 `describe.en.md`（移除「`--exact-match`/`--candidates 0` 下尚未追加 `^0`」的说明），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC3–AC6 的夹具：`create_committed_repo_via_cli()` 后新增提交 `C1` 并 `libra tag -m v1.0 v1.0`（附注，指向 `C1`），再新增提交 `C2` 并 `libra tag lw`（轻量，指向 `C2`）；「新增提交 `X`」按「共享测试夹具」的约定；所有命令须退出 0（执行条件））

- AC1、AC2 → 修改既有 `command::describe_test::test_describe_contains_exact_match_requires_direct_tag`：其 `:687` 循环中 `--exact-match` 与 `--candidates 0` 两种拼写的期望由 `v1.0` 改为 `v1.0^0`（各一次比较；同一用例后半对未被直接指向的祖先仍须失败的既有断言不变）。
- AC3 → `(new)` `command::describe_test::contains_exact_match_flag_appends_caret_zero`：`libra describe --contains --exact-match <C2>` 的 stdout 恰为 `lw`（把 `^0` 加到所有 tag 的错误实现输出 `lw^0`）。
- AC4、AC5、AC6 → `(new)` 同 AC3 用例：`libra describe --contains --exact-match --json <C1>` 的 `data.result == "v1.0^0"`、`data.tag == "v1.0"`、`data.exact_match == true`（各一次比较；把 `^0` 并入 tag 名的错误实现使 `data.tag` 为 `v1.0^0`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `describe`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::describe_test::contains_exact_match_flag_appends_caret_zero) or test(=command::describe_test::test_describe_contains_exact_match_requires_direct_tag)'`（第一条 `(new)`，第二条为本卡修改的既有用例；`cargo nextest list` 须恰好列出这 2 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::describe_test::/) or test(/^command::describe_long_test::/)'`

**Full-suite trigger:** `T-1：本卡 patch + 1 修改 install.sh / install.ps1 版本面（C 组第 ④ 步在 bump 后跑双环境完整 nextest）` | **Dependencies:** `DESC-01, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/command/describe.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md` 与 `COMPATIBILITY.md`（`:219` `describe` 行原地更新，不改行数），说明 `--contains --exact-match`/`--candidates 0` 对附注 tag 精确匹配同样输出 `<tag>^0`；网站页由 `python3 scripts/sync-libra-command-docs.py --write --only describe` 生成（命令全文见「文档与网站验收命令」第 3 步）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M`（修改公开 CLI 输出：一处公开行为变化；模板 G-04 的 `S` 不允许公开行为变化，`plan-template.md:490-491`） | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--contains --exact-match 分支附注 tag ^0; recovery=immutable-release：前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-01,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-01; release=independent; split-from=DESC-01; exception=N/A`

---

### Task DESC-02: 为 `libra describe` 增加 `--no-match` 并支持重置 `--match` 过滤（#796）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1；Git CLI 选项兼容 | **Issue:** [#796](https://github.com/libra-tools/libra/issues/796)

**Description:** 在 `DescribeArgs` 中增加 `--no-match`，按 `ADR-DESC-01` 以 clap `overrides_with` 实现命令行顺序语义：清除此前所有 `--match` 模式、保留其后新传入的 `--match`，不影响 `--exclude`；同步更新 `describe_commit_for_atom` 的结构体字面量（`src/command/describe.rs:159-173`）。

**Out of scope:** `--all` 分支过滤（由 `DESC-03` 承接）；`--no-exclude`（不在 `#796` 范围内，尚未排期，重启条件：出现对应 Issue）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DescribeArgs` 仅有 `match_patterns: Vec<String>`，无 `--no-match` | `src/command/describe.rs:102-104` |
| `describe_commit_for_atom` 构造 `DescribeArgs` 字面量 | `src/command/describe.rs:159-173` |
| `compile_globs(&args.match_patterns)` 直接编译全部 `--match` | `src/command/describe.rs:216` |

**Acceptance criteria:**

- [ ] AC1：`libra describe --match 'v1.*' --match 'v0.*' --no-match` 清空此前全部 `--match`，输出最近 tag 的描述（夹具下为 `v2.0-1-g<ab7(C3)>`）。
- [ ] AC2：`libra describe --match 'v2.*' --no-match --match 'v1.*'` 只按 `--no-match` 之后的 `v1.*` 过滤。
- [ ] AC3：`libra describe --contains --match=B --no-match <commit>`（`#796` / `t6120#66` 上游用例形态）以附注 tag 名加 `^0` 命中。
- [ ] AC4：单独 `libra describe --no-match` 合法并输出最近 tag 的描述（夹具下为 `v2.0-1-g<ab7(C3)>`）。
- [ ] AC5：`libra describe --match 'v*' --no-match --exclude 'v2.*'` 仍应用其后的 `--exclude`。
- [ ] AC6：`libra describe --exclude 'v2.*' --no-match` 仍应用其前的 `--exclude`（`--no-match` 只清除 `--match`）。
- [ ] AC7：（解析层）`DescribeArgs::try_parse_from(["describe", "--match", "v1.*", "--match", "v0.*", "--no-match"])` 的 `match_patterns` 为空。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219` 与网站 `describe.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC1–AC6 由 `(new)` `command::describe_test::no_match_resets_previous_match_patterns` 承载。夹具：`create_committed_repo_via_cli()` 后依次新增提交 `C1`（`libra tag -m v1.0 v1.0`）、`C2`（`libra tag -m v2.0 v2.0`）、`C3`（`HEAD`）；`ab7(C3)` 为 `C3` 的前 7 位；所有命令须退出 0（执行条件）。前置断言：`libra describe --match 'v1.*'` 的 stdout 为 `v1.0-2-g<ab7(C3)>`，`libra describe` 为 `v2.0-1-g<ab7(C3)>`）

- AC1：`libra describe --match 'v1.*' --match 'v0.*' --no-match` 的 stdout 恰为 `v2.0-1-g<ab7(C3)>`（只清除最近一个模式而保留 `v1.*` 的错误实现输出 `v1.0-2-g…`；只保留 `v0.*` 的错误实现以 `LBR-REPO-003` 失败）。
- AC2：`libra describe --match 'v2.*' --no-match --match 'v1.*'` 的 stdout 恰为 `v1.0-2-g<ab7(C3)>`（保留 `v2.*` 的错误实现输出 `v2.0-1-g…`）。
- AC3：`libra describe --contains --match=B --no-match <C1>` 的 stdout 恰为 `v1.0^0`（保留 `B` 过滤的错误实现以 `LBR-REPO-003` 失败）。
- AC4：`libra describe --no-match` 的 stdout 恰为 `v2.0-1-g<ab7(C3)>`。
- AC5：`libra describe --match 'v*' --no-match --exclude 'v2.*'` 的 stdout 恰为 `v1.0-2-g<ab7(C3)>`。
- AC6：`libra describe --exclude 'v2.*' --no-match` 的 stdout 恰为 `v1.0-2-g<ab7(C3)>`（`--no-match` 同时清空已有 `--exclude` 的错误实现输出 `v2.0-1-g…`）。
- AC7 → `(new)` 纯函数单元用例 `command::describe::tests::no_match_parse_order`：`try_parse_from` 返回 `Ok`，`match_patterns` 一次比较为空（CLAUDE.md 要求新增选项配 focused 单元测试；不读写仓库、不改 cwd/env）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `describe`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::describe_test::no_match_resets_previous_match_patterns)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 卡特有用例（AC7，解析层）：`source .env.test && cargo nextest run --lib -E 'test(=command::describe::tests::no_match_parse_order)'`（`(new)`；`cargo nextest list --lib` 须恰好列出该 1 条）
- [ ] 非 nextest 门（TC-1034 / `t6120#66`）：`export TC_RUN_ID="desc-02-$(date -u +%Y%m%dT%H%M%SZ)-$$" TC_TMP_ROOT=/Volumes/Data/tmp/libra-testcases && cargo build --bin libra && LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1034-describe-contains/tc1034-describe-contains.sh`（判据与记录项按 `DEP-DESC-TC`）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::describe_test::/) or test(/^command::describe_long_test::/)'`；`source .env.test && cargo nextest run --lib -E 'test(/^command::describe::tests::/)'`

**Full-suite trigger:** `T-1：本卡 patch + 1 修改 install.sh / install.ps1 版本面（C 组第 ④ 步在 bump 后跑双环境完整 nextest）` | **Dependencies:** `DESC-01a, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC`

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md` 与 `COMPATIBILITY.md:219`（原地），补充 `--no-match` 及其命令行顺序语义；网站页按「文档与网站验收命令」第 3 步以 `--only describe` 生成。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M`（新增 1 处公开 CLI 标志） | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--no-match 重置 --match 过滤; recovery=immutable-release：前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=7/8; VER=3/8; landing=1; prod-files=1; scope=M; deps=DESC-01a,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-01a; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-03: 修复 `libra describe --all` 忽略 `--match`/`--exclude` 过滤（#797）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1；Git 过滤语义缺陷 | **Issue:** [#797](https://github.com/libra-tools/libra/issues/797)

**Description:** 使 `--all` 收集的本地分支与远程跟踪分支同样经过 `--match`/`--exclude` 过滤：非 `--contains` 路径按 Git `get_name()` 以去掉 `refs/heads/`、`refs/remotes/` 后的短名（如 `br-x`、`o/main`）匹配；`--contains --all` 按 Git 传给 `name-rev` 的 `--refs=refs/{tags,heads,remotes}/<pat>` 等价规则匹配；无候选时按既有错误返回。过滤沿用既有 `compile_globs`/`tag_passes_filters`（`wax`）语义，只把作用范围扩到分支候选；glob 中 `*` 是否跨 `/` 与 Git `wildmatch` 的差异不在本卡改变（`DEFER-DESC-07`）。

**Out of scope:** `--no-match` 参数解析（`DESC-02`）；`--contains --all` 输出去掉 `heads/` 前缀（`DEFER-DESC-03`）；glob `*` 跨 `/` 语义（`DEFER-DESC-07`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| tag 候选调用 `tag_passes_filters(&t.name, &matchers, &excluders)` | `src/command/describe.rs:232-234` |
| `if include_all` 分支直接插入 `heads/...` 与 `remotes/...`，未调用过滤函数 | `src/command/describe.rs:265-289` |
| `NoContainingTag` 映射 `LBR-REPO-003` | `src/command/describe.rs:646-652`，`src/utils/error.rs:378` |

**Acceptance criteria:**

- [ ] AC1：`libra describe --contains --all --match 'zzz*' <commit>` 在无任何 ref 匹配时以 `LBR-REPO-003` 失败（`#797` / `t6120#67`）。
- [ ] AC2：`libra describe --all --match 'br-*' <commit>`（非 `--contains`）按本地分支短名匹配。
- [ ] AC3：`libra describe --all --exclude 'main' --exclude 'o/*' <commit>`（非 `--contains`）按本地与远程跟踪分支短名排除。
- [ ] AC4：`libra describe --all --match 'o/r*' <commit>`（非 `--contains`）按远程跟踪分支短名 `<remote>/<branch>` 匹配。
- [ ] AC5：`libra describe --contains --all --match 'br-*' <commit>` 按 `refs/heads/<pat>` 等价规则命中本地分支。
- [ ] AC6：tag 与分支同时通过过滤并指向同一提交时，`libra describe --all --match` 输出 tag 名（夹具下为 `tags/rel`）。
- [ ] AC7：不带 `--match`/`--exclude` 时，`libra describe --all <commit>` 对被本地分支直接指向的提交输出 `heads/<branch>`（夹具下为 `heads/main`）。
- [ ] AC8：`libra describe --contains --all --match 'br-*' --exclude 'br-x' <commit>` 在唯一匹配的分支被排除时以 `LBR-REPO-003` 失败（`--exclude` 同样作用于 `--contains --all` 的分支候选）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219` 与网站 `describe.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::describe_test::all_candidates_respect_match_and_exclude` 承载。夹具：`create_committed_repo_via_cli()` 后在 `main` 上依次新增提交 `C1`、`C2`、`C3`（`main` 指向 `C3`）；`libra tag -m v1 v1 <C1>`、`libra branch br-x <C2>`、`libra tag -m rel rel <C2>`、`libra branch rel-b <C2>`；以 `libra clone <主仓库路径> <S>` 得到同历史仓库 `S`，在 `S` 中 `libra branch rb <C2>`，再在主仓库执行 `libra remote add o <S>` 与 `libra fetch o`，得 `refs/remotes/o/main` = `C3`、`refs/remotes/o/rb` = `C2`；`ab7(X)` 为前 7 位；成功类命令须退出 0（执行条件），失败类命令须非 0 退出（执行条件））

- AC1：`libra describe --contains --all --match 'zzz*' <C1>` 的错误码为 `LBR-REPO-003`（`parse_cli_error_stderr` 的 `error_code`）。
- AC2：`libra describe --all --match 'br-*' <C3>` 的 stdout 恰为 `heads/br-x-1-g<ab7(C3)>`（以 `heads/br-x` 全名匹配的错误实现无候选；不过滤分支的现状输出 `heads/main`）。
- AC3：`libra describe --all --exclude 'main' --exclude 'o/*' <C3>` 的 stdout 恰为 `tags/rel-1-g<ab7(C3)>`（`C3` 上的 `main`、`o/main` 均被排除，`C2` 上附注 tag 优先；以全名匹配排除的错误实现或不过滤分支的现状输出 `heads/main`）。
- AC4：`libra describe --all --match 'o/r*' <C3>` 的 stdout 恰为 `remotes/o/rb-1-g<ab7(C3)>`（以 `remotes/o/rb` 全名匹配的错误实现无候选）。
- AC5：`libra describe --contains --all --match 'br-*' <C1>` 的 stdout 恰为 `heads/br-x~1`（pinned Git 输出 `br-x~1`，前缀差异见 `DEFER-DESC-03`；不过滤的现状命中 `tags/v1^0`）。
- AC6：`libra describe --all --match 'rel*' <C2>` 的 stdout 恰为 `tags/rel`。
- AC7：`libra describe --all <C3>` 的 stdout 恰为 `heads/main`。
- AC8：`libra describe --contains --all --match 'br-*' --exclude 'br-x' <C1>` 的错误码为 `LBR-REPO-003`（pinned Git 同样失败；`--contains` 路径不应用 `--exclude` 的错误实现输出 `heads/br-x~1`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `describe`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::describe_test::all_candidates_respect_match_and_exclude)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 非 nextest 门（TC-1035 / `t6120#67`）：`export TC_RUN_ID="desc-03-$(date -u +%Y%m%dT%H%M%SZ)-$$" TC_TMP_ROOT=/Volumes/Data/tmp/libra-testcases && cargo build --bin libra && LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1035-describe-contains/tc1035-describe-contains.sh`（判据与记录项按 `DEP-DESC-TC`）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::describe_test::/) or test(/^command::describe_long_test::/)'`

**Full-suite trigger:** `T-1：本卡 patch + 1 修改 install.sh / install.ps1 版本面（C 组第 ④ 步在 bump 后跑双环境完整 nextest）` | **Dependencies:** `DESC-02, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC`

**Implementation write set:** `src/command/describe.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md` 与 `COMPATIBILITY.md:219`（原地），明确 `--match`/`--exclude` 同样作用于 `--all` 的本地分支与远程跟踪分支（短名规则）；网站页按「文档与网站验收命令」第 3 步以 `--only describe` 生成。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M`（修改公开 CLI 过滤行为） | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--all 候选受 --match/--exclude 过滤; recovery=immutable-release：前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DESC-02,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-02; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-09: 在 `rev-parse` 中支持 `<ref>-<count>-g<hex>` 后缀解析 commit（`#798`/`#799` 往返前置）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1；`#798` 与 `#799` 往返验收前置 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 按 `ADR-DESC-03` 新增共享 describe 后缀 helper（含语法函数 `split_describe_name`），接入 `resolve_object_atom_typed`（`src/utils/util.rs:2404-2468`）的根命名空间路径：tag、本地分支、远程分支三级字面查找全部未命中后、纯十六进制回退之前，按 Git 语法识别 `<ref>-<count>-g<hex>`（`<count>` 为 ≥ 1 个十六进制字符；`<ref>` 非空、不等于 `@` 且 `util::is_valid_refname(&format!("refs/{ref}"))` 为真；`<hex>` 交由既有 `resolve_hash_atom_typed` 校验字符与长度，`src/utils/util.rs:2319-2335`），并解析为 commit。本卡是家族 `REL-DESC-09` 的子卡（1/16）：只本地签名提交、不发布；按家族实现顺序本卡只放行 commit，缺失/歧义细则（`DESC-09b`）、附注 tag（`DESC-09c`）与 blob/tree（`DESC-09a`）由后续子卡在同一家族内补齐，经发布点 `DESC-11` 一次对外发布，故不设临时边界判据（`ADR-DESC-03` ③）；`tags/`、`heads/`、`remotes/` 及其 `refs/` 形式的早返回本卡不变，由同一家族的 `DESC-10..10k` 接入。

**Out of scope:** 缺失与歧义语义（`DESC-09b`）；附注 tag 对象（`DESC-09c`）；blob/tree（`DESC-09a`）；六种命名空间前缀（按 G-09 自本卡拆出 `DESC-10..10e`、`DESC-10f..10k`）；`describe` 输出逻辑（`DESC-05`/`DESC-04`）；大写十六进制（`DEFER-DESC-05`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_object_atom_typed` 依次尝试 tag、本地分支、远程分支，最后才走纯十六进制，不识别 describe 后缀 | `src/utils/util.rs:2404-2468`（字面三级 `:2457-2466`，纯十六进制回退 `:2468`） |
| 纯十六进制回退：全长不校验存在性，缩写缺失/歧义分别返回 `InvalidReference` / `Ambiguous`（均映射 `LBR-CLI-003`） | `src/utils/util.rs:2319-2393`，`src/command/rev_parse.rs:997-1010` |
| pinned Git 的 describe 名语法与消歧 | `ADR-DESC-03`（`object-name.c:1028-1081` 于 pinned 构建缓存核对） |

**Acceptance criteria:**

- [ ] AC1：无同名字面引用时，`libra rev-parse v1-1-g<commit 的 8 位缩写>` 输出该 commit 的完整 OID。
- [ ] AC2：无同名字面引用时，`libra rev-parse x-1a-g<commit 全长 OID>` 输出该 commit（计数段可含十六进制字母）。
- [ ] AC3：存在同名且后缀合法的远程跟踪分支时，解析为该分支目标而非后缀对象（远程是 `resolve_object_atom_typed` 最后一级字面查找，`src/utils/util.rs:2457-2466`，故 helper 只要排在全部字面查找之后即同时满足 tag 与本地分支的字面优先）。
- [ ] AC4：存在后缀语法无效（`-gxyz`）的同名远程跟踪分支时，解析为该分支目标而非失败。
- [ ] AC5：缺少计数段（`x-g<hex>`）的名字以 `LBR-CLI-003` 失败。
- [ ] AC6：计数段含非十六进制字符（`x-z-g<hex>`）的名字以 `LBR-CLI-003` 失败。
- [ ] AC7：`<ref>` 为非法 ref 名（`x.lock-1-g<hex>`）的名字以 `LBR-CLI-003` 失败。
- [ ] AC8：`<ref>` 为单独 `@`（`@-1-g<hex>`）的名字以 `LBR-CLI-003` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 、`COMPATIBILITY.md:217` 及网站 `rev-parse.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::rev_parse_test::describe_suffix_commit` 承载；夹具 `FX-DESC-RP`（`ADR-DESC-03`）的 `P`/`C`/`B`、远程 `o`（`S` 的提交 `SC`）；`<hex>` 一律取 `ab(C)`，使「不做该项语法检查」的错误实现输出 `C`；成功类命令须退出 0、失败类命令须非 0 退出（执行条件）；错误码取 `parse_cli_error_stderr` 的 `error_code`）

- AC1：`libra rev-parse v1-1-g<ab(C)>` 的 stdout 恰为 `C`（现状以 `LBR-CLI-003` 失败）。
- AC2：`libra rev-parse x-1a-g<C>` 的 stdout 恰为 `C`（只接受十进制计数的错误实现失败）。
- AC3：在 `S` 中建分支 `rlit-1-g<ab(B)>` 指向 `SC` 并 `libra fetch o` 后，`libra rev-parse o/rlit-1-g<ab(B)>` 的 stdout 恰为 `SC`（helper 排在任一字面查找之前的错误实现得到 `B` 或失败）。
- AC4：在 `S` 中建分支 `rbad-1-gxyz` 指向 `SC` 并 `libra fetch o` 后，`libra rev-parse o/rbad-1-gxyz` 的 stdout 恰为 `SC`（先做语法检查即报错的错误实现失败）。
- AC5–AC8：`libra rev-parse x-g<ab(C)>`、`x-z-g<ab(C)>`、`x.lock-1-g<ab(C)>`、`@-1-g<ab(C)>` 的错误码均为 `LBR-CLI-003`（各一次比较）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_commit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-03, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `<ref>-<count>-g<hex>` 语法与字面引用优先（家族发布前不对外发布）；EN 文档新增单独成行的「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（1/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=根命名空间 describe 后缀语法与 commit 解析; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-03,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-03; release=family child; split-from=N/A; exception=N/A`

---

### Task DESC-09b: 后缀 OID 的无效、缺失与歧义按 Git 语义处理

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（2/16）。按 `ADR-DESC-03` ② 为 describe 后缀 helper 实现 Git `GET_OID_QUIETLY | GET_OID_COMMIT` 语义：`<hex>` 含非十六进制字符或少于 4 位时经既有 `resolve_hash_atom_typed` 以 `LBR-CLI-003` 失败；后缀缩写/全长对象不存在时以 `InvalidReference`（`LBR-CLI-003`）失败（全长后缀须校验存在性，不同于纯 40 位十六进制）；歧义前缀中恰有一个 commit 候选时返回该 commit；其余歧义以 `LBR-CLI-003` 失败（实现上返回 `InvalidReference`、不输出候选提示以贴近 Git；提示文案不设判据）。不改变字面引用优先与 commit 放行范围。

**Out of scope:** 非提交对象（`DESC-09c`、`DESC-09a`）；命名空间前缀（`DESC-10..10k`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_hash_atom_typed` 的歧义分支返回带候选的 `Ambiguous`；全长分支不校验存在性 | `src/utils/util.rs:2319-2327, 2351-2380` |
| pinned Git 实测：`x-1-g92ae10d0`（commit+blob）→ commit；`x-1-gc3da9f35`（两 commit）、`x-1-gf6a3ecac`（两 blob）、`x-1-g00000000`、`x-1-g<40 个 0>` 均以 `unknown revision` 失败且无歧义提示 | `ADR-DESC-03` |

**Acceptance criteria:**

- [ ] AC1：后缀为不存在对象的缩写（`00000000`）时以 `LBR-CLI-003` 失败。
- [ ] AC2：后缀为不存在对象的全长 OID（40 个 `0`）时以 `LBR-CLI-003` 失败。
- [ ] AC3：后缀前缀同时匹配一个 commit 与一个 blob 时解析为该 commit。
- [ ] AC4：后缀前缀匹配两个 commit 时以 `LBR-CLI-003` 失败。
- [ ] AC5：后缀前缀匹配两个 blob 时以 `LBR-CLI-003` 失败。
- [ ] AC6：无同名字面引用且 `<hex>` 含非十六进制字符（`x-1-gzzzzzzzz`）时以 `LBR-CLI-003` 失败。
- [ ] AC7：无同名字面引用且 `<hex>` 少于 4 位（`x-1-g<ab(C) 前 3 位>`）时以 `LBR-CLI-003` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 、`COMPATIBILITY.md:217` 及网站 `rev-parse.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::rev_parse_test::describe_suffix_missing_and_ambiguous` 承载；夹具 `FX-DESC-RP` 的缺失对象与三组歧义常量及其前置断言；成功类命令须退出 0、失败类命令须非 0 退出（执行条件）；错误码取 `parse_cli_error_stderr` 的 `error_code`）

- AC1：`libra rev-parse x-1-g00000000` 的错误码为 `LBR-CLI-003`。
- AC2：`libra rev-parse x-1-g0000000000000000000000000000000000000000` 的错误码为 `LBR-CLI-003`（按纯 40 位不校验存在性处理的错误实现输出全 0）。
- AC3：`libra rev-parse x-1-g92ae10d0` 的 stdout 恰为 `92ae10d00618309f4ecba5b1735d693bc74ceaf5`（沿用纯十六进制歧义报错的现状失败）。
- AC4：`libra rev-parse x-1-gc3da9f35` 的错误码为 `LBR-CLI-003`（任取其一的错误消歧输出某个 commit）。
- AC5：`libra rev-parse x-1-gf6a3ecac` 的错误码为 `LBR-CLI-003`（任取其一的错误消歧输出某个 blob）。
- AC6、AC7：`libra rev-parse x-1-gzzzzzzzz`、`libra rev-parse x-1-g<ab(C) 前 3 位>` 的错误码均为 `LBR-CLI-003`（各一次比较；截断或放宽 `<hex>` 校验、把前 3 位当作 `C` 前缀的错误实现输出 `C`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_missing_and_ambiguous)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-09, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 describe 后缀的缺失、commit 优先消歧与 `LBR-CLI-003` 失败；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（2/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=describe 后缀 OID 无效、缺失与歧义语义; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-09,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-09; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-09c: 根命名空间后缀解析附注 tag 对象

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（3/16）。根命名空间 describe 后缀解析到附注 tag 对象时返回该 tag 对象自身（不剥皮为 commit，对齐 Git）；blob/tree 由同一家族的 `DESC-09a` 放开（家族内实现顺序，不设临时边界判据）。

**Out of scope:** blob/tree（`DESC-09a`）；命名空间前缀（`DESC-10..10k`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DESC-09` 交付后 helper 只放行 commit | `ADR-DESC-03` ③ |
| pinned Git 实测：`tags/q-3-g<8 位 tag 对象>` 返回 tag 对象自身 | `ADR-DESC-03` |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse x-1-g<附注 tag 对象的 8 位缩写>` 输出该 tag 对象自身的 OID。
- [ ] AC2：`libra rev-parse x-1-g<附注 tag 对象全长 OID>` 输出该 tag 对象自身的 OID。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 、`COMPATIBILITY.md:217` 及网站 `rev-parse.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::rev_parse_test::describe_suffix_tag_object` 承载；夹具 `FX-DESC-RP` 的 `G`（`v1` 的 tag 对象）与 `P`；命令须退出 0（执行条件））

- AC1：`libra rev-parse x-1-g<ab(G)>` 的 stdout 恰为 `G`（剥皮的错误实现输出 `P`，未放行的现状以 `LBR-CLI-003` 失败）。
- AC2：`libra rev-parse x-1-g<G>` 的 stdout 恰为 `G`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC2）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_tag_object)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-09b, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明根命名空间后缀可解析附注 tag 对象（不剥皮）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（3/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=根命名空间后缀放行附注 tag 对象; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-09b,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-09b; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-09a: 根命名空间后缀解析 blob 与 tree

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（4/16）。根命名空间 describe 后缀放开 blob 与 tree（返回对象自身），至此根命名空间与 Git 的对象类型范围一致；歧义、缺失与 tag 对象语义保持 `DESC-09b`/`DESC-09c` 的结果（由其用例作共享回归）。

**Out of scope:** 命名空间前缀（`DESC-10..10k`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DESC-09c` 交付后 helper 在根命名空间放行 commit 与附注 tag | `ADR-DESC-03` ③ |
| pinned Git 实测：`nonsense-1-g<8 位 blob>`、`heads/x-1-g<8 位 tree>` 返回对象自身 | `ADR-DESC-03` |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse x-1-g<blob 的 8 位缩写>` 输出该 blob 的 OID。
- [ ] AC2：`libra rev-parse x-1-g<blob 全长 OID>` 输出该 blob 的 OID。
- [ ] AC3：`libra rev-parse x-1-g<tree 的 8 位缩写>` 输出该 tree 的 OID。
- [ ] AC4：`libra rev-parse x-1-g<tree 全长 OID>` 输出该 tree 的 OID。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 、`COMPATIBILITY.md:217` 及网站 `rev-parse.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::rev_parse_test::describe_suffix_blob_and_tree` 承载；夹具 `FX-DESC-RP` 的 `B`、`T`；命令须退出 0（执行条件））

- AC1：`libra rev-parse x-1-g<ab(B)>` 的 stdout 恰为 `B`（未放行的现状以 `LBR-CLI-003` 失败）。
- AC2：`libra rev-parse x-1-g<B>` 的 stdout 恰为 `B`。
- AC3：`libra rev-parse x-1-g<ab(T)>` 的 stdout 恰为 `T`。
- AC4：`libra rev-parse x-1-g<T>` 的 stdout 恰为 `T`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC4）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_blob_and_tree)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-09c, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明根命名空间后缀可解析 blob 与 tree；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（4/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=根命名空间后缀放行 blob 与 tree; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-09c,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-09c; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-10: `tags/` 命名空间 describe 后缀解析 commit

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（5/16）。在 `tags/`/`refs/tags/` 早返回分支（`src/utils/util.rs:2438-2443`）中，对以 `tags/` 开头的名字：本命名空间字面查找未命中且名字符合 describe 语法时，先按 `ADR-DESC-03` ①「字面查找顺序」以完整名字查找 tag、本地分支与远程跟踪引用，仍未命中才调用 describe 后缀 helper（含 `DESC-09b` 的缺失与 commit 优先消歧语义）解析 commit；字面引用优先。非提交对象由同家族的 `DESC-10f` 放开、其它前缀由同家族各自的卡接入（家族内实现顺序：中间态只存在于本地子卡提交、不对外发布，故不设临时边界判据，见 `REL-DESC-09`）。

**Out of scope:** `tags/` 的非提交对象（`DESC-10f`）；其它命名空间（各自的 `DESC-10*` 卡）；`describe` 输出生成。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `tags/` 名字在字面查找未命中时直接返回 `invalid_reference` | `tags/`/`refs/tags/` 早返回（`src/utils/util.rs:2438-2443`） |
| pinned Git 对任意前缀（含 `tags/`）的 describe 名按同一规则解析、歧义时 commit 优先 | `ADR-DESC-03` |
| pinned Git 在解析 describe 名之前按 `ref_rev_parse_rules` 以完整名字查找其它命名空间的引用 | `ADR-DESC-03` Context（`refs.c:643-651`、`:827-835`；`object-name.c:1131`、`:1136`） |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse tags/x-1-g<commit 的 8 位缩写>` 输出该 commit。
- [ ] AC2：`libra rev-parse tags/x-1-g<commit 全长 OID>` 输出该 commit。
- [ ] AC3：存在同名字面引用 `tags/tlit-1-g<ab(B)>` 时解析为其目标而非后缀对象。
- [ ] AC4：`libra rev-parse tags/x-1-g92ae10d0`（commit 与 blob 共享前缀）解析为该 commit。
- [ ] AC5：`libra rev-parse tags/x-1-g<C 全长>~1` 在后缀原子之上继续解析，输出 `C` 的父提交。
- [ ] AC6：只存在以完整名字 `tags/dw-1-g<ab(C)>` 命名的本地分支（指向 `P`）、本命名空间无同名引用时，`libra rev-parse tags/dw-1-g<ab(C)>` 输出 `P`（跨命名空间完整名字查找，`ADR-DESC-03` ①「字面查找顺序」）。
- [ ] AC7：只存在以完整名字 `tags/dwr-1-g<ab(C)>` 对应的远程跟踪分支（远程 `tags` 的分支 `dwr-1-g<ab(C)>`，指向 `SC`）时，`libra rev-parse tags/dwr-1-g<ab(C)>` 输出 `SC`（跨命名空间完整名字查找的远程跟踪级，`ADR-DESC-03` ①「字面查找顺序」）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 与网站 `rev-parse.en.md`（说明 `tags/` 前缀的 describe 名解析 commit），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC1–AC7 由 `(new)` `command::rev_parse_test::describe_suffix_ns_tags_commit` 承载；夹具 `FX-DESC-RP`（含远程 `o`/`S`/`SC` 与歧义常量）；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1：stdout 恰为 `C`（现状以 `LBR-CLI-003` 失败）。
- AC2：stdout 恰为 `C`。
- AC3：`libra tag tlit-1-g<ab(B)> <P>` 后，`libra rev-parse tags/tlit-1-g<ab(B)>` 的 stdout 恰为 `P`（先解析后缀的错误实现得到 `B` 或失败）。
- AC4：stdout 恰为 `92ae10d00618309f4ecba5b1735d693bc74ceaf5`。
- AC5：`libra rev-parse tags/x-1-g<C>~1` 的 stdout 恰为 `P`（pinned Git 对同构名字返回父提交，见 `ADR-DESC-03`）。
- AC6：`libra branch tags/dw-1-g<ab(C)> <P>`（本地分支，对应 Git `refs/heads/tags/…`）后，`libra rev-parse tags/dw-1-g<ab(C)>` 的 stdout 恰为 `P`（只查本命名空间即进入后缀 helper 的错误实现得到 `C`；现状以 `LBR-CLI-003` 失败；pinned Git 对同构夹具返回 `P`，见 `ADR-DESC-03` Context）。
- AC7：在 `S` 中建分支 `dwr-1-g<ab(C)>` 指向 `SC`，`libra remote add tags <S 路径>` 与 `libra fetch tags` 后，`libra rev-parse tags/dwr-1-g<ab(C)>` 的 stdout 恰为 `SC`（不查远程跟踪名的错误实现得到 `C`；pinned Git 对同构夹具返回远程分支目标，见 `ADR-DESC-03` Context）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_ns_tags_commit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-09a, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `tags/` 前缀的 describe 名解析 commit（含字面引用优先、跨命名空间完整名字查找与 commit 优先消歧）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（5/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=tags/ 命名空间后缀解析 commit; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-09a,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-09a; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-10a: `heads/` 命名空间 describe 后缀解析 commit

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（6/16）。在 `heads/`/`refs/heads/` 早返回分支（`src/utils/util.rs:2444-2449`）中，对以 `heads/` 开头的名字：本命名空间字面查找未命中且名字符合 describe 语法时，先按 `ADR-DESC-03` ①「字面查找顺序」以完整名字查找 tag、本地分支与远程跟踪引用，仍未命中才调用 describe 后缀 helper（含 `DESC-09b` 的缺失与 commit 优先消歧语义）解析 commit；字面引用优先。非提交对象由同家族的 `DESC-10g` 放开、其它前缀由同家族各自的卡接入（家族内实现顺序：中间态只存在于本地子卡提交、不对外发布，故不设临时边界判据，见 `REL-DESC-09`）。

**Out of scope:** `heads/` 的非提交对象（`DESC-10g`）；其它命名空间（各自的 `DESC-10*` 卡）；`describe` 输出生成。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `heads/` 名字在字面查找未命中时直接返回 `invalid_reference` | `heads/`/`refs/heads/` 早返回（`src/utils/util.rs:2444-2449`） |
| pinned Git 对任意前缀（含 `heads/`）的 describe 名按同一规则解析、歧义时 commit 优先 | `ADR-DESC-03` |
| pinned Git 在解析 describe 名之前按 `ref_rev_parse_rules` 以完整名字查找其它命名空间的引用 | `ADR-DESC-03` Context（`refs.c:643-651`、`:827-835`；`object-name.c:1131`、`:1136`） |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse heads/x-1-g<commit 的 8 位缩写>` 输出该 commit。
- [ ] AC2：`libra rev-parse heads/x-1-g<commit 全长 OID>` 输出该 commit。
- [ ] AC3：存在同名字面引用 `heads/blit-1-g<ab(B)>` 时解析为其目标而非后缀对象。
- [ ] AC4：`libra rev-parse heads/x-1-g92ae10d0`（commit 与 blob 共享前缀）解析为该 commit。
- [ ] AC5：只存在以完整名字 `heads/dw-1-g<ab(C)>` 命名的轻量 tag（指向 `P`）、本命名空间无同名引用时，`libra rev-parse heads/dw-1-g<ab(C)>` 输出 `P`（跨命名空间完整名字查找，`ADR-DESC-03` ①「字面查找顺序」）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 与网站 `rev-parse.en.md`（说明 `heads/` 前缀的 describe 名解析 commit），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC1–AC5 由 `(new)` `command::rev_parse_test::describe_suffix_ns_heads_commit` 承载；夹具 `FX-DESC-RP`（含远程 `o`/`S`/`SC` 与歧义常量）；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1：stdout 恰为 `C`（现状以 `LBR-CLI-003` 失败）。
- AC2：stdout 恰为 `C`。
- AC3：`libra branch blit-1-g<ab(B)> <P>` 后，`libra rev-parse heads/blit-1-g<ab(B)>` 的 stdout 恰为 `P`（先解析后缀的错误实现得到 `B` 或失败）。
- AC4：stdout 恰为 `92ae10d00618309f4ecba5b1735d693bc74ceaf5`。
- AC5：`libra tag heads/dw-1-g<ab(C)> <P>`（轻量 tag，对应 Git `refs/tags/heads/…`）后，`libra rev-parse heads/dw-1-g<ab(C)>` 的 stdout 恰为 `P`（只查本命名空间即进入后缀 helper 的错误实现得到 `C`；现状以 `LBR-CLI-003` 失败；pinned Git 对同构夹具返回 `P`，见 `ADR-DESC-03` Context）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_ns_heads_commit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-10, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `heads/` 前缀的 describe 名解析 commit（含字面引用优先、跨命名空间完整名字查找与 commit 优先消歧）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（6/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=heads/ 命名空间后缀解析 commit; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-10,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-10; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-10b: `remotes/` 命名空间 describe 后缀解析 commit

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（7/16）。在 `remotes/`/`refs/remotes/` 早返回分支（`src/utils/util.rs:2450-2455`）中，对以 `remotes/` 开头的名字：本命名空间字面查找未命中且名字符合 describe 语法时，先按 `ADR-DESC-03` ①「字面查找顺序」以完整名字查找 tag、本地分支与远程跟踪引用，仍未命中才调用 describe 后缀 helper（含 `DESC-09b` 的缺失与 commit 优先消歧语义）解析 commit；字面引用优先。非提交对象由同家族的 `DESC-10h` 放开、其它前缀由同家族各自的卡接入（家族内实现顺序：中间态只存在于本地子卡提交、不对外发布，故不设临时边界判据，见 `REL-DESC-09`）。

**Out of scope:** `remotes/` 的非提交对象（`DESC-10h`）；其它命名空间（各自的 `DESC-10*` 卡）；`describe` 输出生成。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `remotes/` 名字在字面查找未命中时直接返回 `invalid_reference` | `remotes/`/`refs/remotes/` 早返回（`src/utils/util.rs:2450-2455`） |
| pinned Git 对任意前缀（含 `remotes/`）的 describe 名按同一规则解析、歧义时 commit 优先 | `ADR-DESC-03` |
| pinned Git 在解析 describe 名之前按 `ref_rev_parse_rules` 以完整名字查找其它命名空间的引用 | `ADR-DESC-03` Context（`refs.c:643-651`、`:827-835`；`object-name.c:1131`、`:1136`） |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse remotes/o/x-1-g<commit 的 8 位缩写>` 输出该 commit。
- [ ] AC2：`libra rev-parse remotes/o/x-1-g<commit 全长 OID>` 输出该 commit。
- [ ] AC3：存在同名字面引用 `remotes/o/rlit-1-g<ab(B)>` 时解析为其目标而非后缀对象。
- [ ] AC4：`libra rev-parse remotes/o/x-1-g92ae10d0`（commit 与 blob 共享前缀）解析为该 commit。
- [ ] AC5：只存在以完整名字 `remotes/o/dw-1-g<ab(C)>` 命名的本地分支（指向 `P`）、本命名空间无同名引用时，`libra rev-parse remotes/o/dw-1-g<ab(C)>` 输出 `P`（跨命名空间完整名字查找，`ADR-DESC-03` ①「字面查找顺序」）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 与网站 `rev-parse.en.md`（说明 `remotes/` 前缀的 describe 名解析 commit），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC1–AC5 由 `(new)` `command::rev_parse_test::describe_suffix_ns_remotes_commit` 承载；夹具 `FX-DESC-RP`（含远程 `o`/`S`/`SC` 与歧义常量）；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1：stdout 恰为 `C`（现状以 `LBR-CLI-003` 失败）。
- AC2：stdout 恰为 `C`。
- AC3：在 `S` 中建分支 `rlit-1-g<ab(B)>` 指向 `SC` 后 `libra fetch o` 后，`libra rev-parse remotes/o/rlit-1-g<ab(B)>` 的 stdout 恰为 `SC`（先解析后缀的错误实现得到 `B` 或失败）。
- AC4：stdout 恰为 `92ae10d00618309f4ecba5b1735d693bc74ceaf5`。
- AC5：`libra branch remotes/o/dw-1-g<ab(C)> <P>`（本地分支，对应 Git `refs/heads/remotes/o/…`；`S` 中无分支 `dw-1-g<ab(C)>`）后，`libra rev-parse remotes/o/dw-1-g<ab(C)>` 的 stdout 恰为 `P`（只查本命名空间即进入后缀 helper 的错误实现得到 `C`；现状以 `LBR-CLI-003` 失败；pinned Git 对同构夹具返回 `P`，见 `ADR-DESC-03` Context）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_ns_remotes_commit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-10a, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `remotes/` 前缀的 describe 名解析 commit（含字面引用优先、跨命名空间完整名字查找与 commit 优先消歧）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（7/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=remotes/ 命名空间后缀解析 commit; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-10a,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-10a; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-10c: `refs/tags/` 命名空间 describe 后缀解析 commit

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（8/16）。在 `tags/`/`refs/tags/` 早返回分支（`src/utils/util.rs:2438-2443`）中，对以 `refs/tags/` 开头的名字：本命名空间字面查找未命中且名字符合 describe 语法时，先按 `ADR-DESC-03` ①「字面查找顺序」以完整名字查找 tag、本地分支与远程跟踪引用，仍未命中才调用 describe 后缀 helper（含 `DESC-09b` 的缺失与 commit 优先消歧语义）解析 commit；字面引用优先。非提交对象由同家族的 `DESC-10i` 放开、其它前缀由同家族各自的卡接入（家族内实现顺序：中间态只存在于本地子卡提交、不对外发布，故不设临时边界判据，见 `REL-DESC-09`）。

**Out of scope:** `refs/tags/` 的非提交对象（`DESC-10i`）；其它命名空间（各自的 `DESC-10*` 卡）；`describe` 输出生成。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `refs/tags/` 名字在字面查找未命中时直接返回 `invalid_reference` | `tags/`/`refs/tags/` 早返回（`src/utils/util.rs:2438-2443`） |
| pinned Git 对任意前缀（含 `refs/tags/`）的 describe 名按同一规则解析、歧义时 commit 优先 | `ADR-DESC-03` |
| pinned Git 在解析 describe 名之前按 `ref_rev_parse_rules` 以完整名字查找其它命名空间的引用 | `ADR-DESC-03` Context（`refs.c:643-651`、`:827-835`；`object-name.c:1131`、`:1136`） |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse refs/tags/x-1-g<commit 的 8 位缩写>` 输出该 commit。
- [ ] AC2：`libra rev-parse refs/tags/x-1-g<commit 全长 OID>` 输出该 commit。
- [ ] AC3：存在同名字面引用 `refs/tags/tlit-1-g<ab(B)>` 时解析为其目标而非后缀对象。
- [ ] AC4：`libra rev-parse refs/tags/x-1-g92ae10d0`（commit 与 blob 共享前缀）解析为该 commit。
- [ ] AC5：只存在以完整名字 `refs/tags/dw-1-g<ab(C)>` 命名的本地分支（指向 `P`）、本命名空间无同名引用时，`libra rev-parse refs/tags/dw-1-g<ab(C)>` 输出 `P`（跨命名空间完整名字查找，`ADR-DESC-03` ①「字面查找顺序」）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 与网站 `rev-parse.en.md`（说明 `refs/tags/` 前缀的 describe 名解析 commit），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC1–AC5 由 `(new)` `command::rev_parse_test::describe_suffix_ns_refs_tags_commit` 承载；夹具 `FX-DESC-RP`（含远程 `o`/`S`/`SC` 与歧义常量）；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1：stdout 恰为 `C`（现状以 `LBR-CLI-003` 失败）。
- AC2：stdout 恰为 `C`。
- AC3：`libra tag tlit-1-g<ab(B)> <P>` 后，`libra rev-parse refs/tags/tlit-1-g<ab(B)>` 的 stdout 恰为 `P`（先解析后缀的错误实现得到 `B` 或失败）。
- AC4：stdout 恰为 `92ae10d00618309f4ecba5b1735d693bc74ceaf5`。
- AC5：`libra branch refs/tags/dw-1-g<ab(C)> <P>`（本地分支，对应 Git `refs/heads/refs/tags/…`）后，`libra rev-parse refs/tags/dw-1-g<ab(C)>` 的 stdout 恰为 `P`（只查本命名空间即进入后缀 helper 的错误实现得到 `C`；现状以 `LBR-CLI-003` 失败；pinned Git 对同构夹具返回 `P`，见 `ADR-DESC-03` Context）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_ns_refs_tags_commit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-10b, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `refs/tags/` 前缀的 describe 名解析 commit（含字面引用优先、跨命名空间完整名字查找与 commit 优先消歧）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（8/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=refs/tags/ 命名空间后缀解析 commit; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-10b,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-10b; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-10d: `refs/heads/` 命名空间 describe 后缀解析 commit

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（9/16）。在 `heads/`/`refs/heads/` 早返回分支（`src/utils/util.rs:2444-2449`）中，对以 `refs/heads/` 开头的名字：本命名空间字面查找未命中且名字符合 describe 语法时，先按 `ADR-DESC-03` ①「字面查找顺序」以完整名字查找 tag、本地分支与远程跟踪引用，仍未命中才调用 describe 后缀 helper（含 `DESC-09b` 的缺失与 commit 优先消歧语义）解析 commit；字面引用优先。非提交对象由同家族的 `DESC-10j` 放开、其它前缀由同家族各自的卡接入（家族内实现顺序：中间态只存在于本地子卡提交、不对外发布，故不设临时边界判据，见 `REL-DESC-09`）。

**Out of scope:** `refs/heads/` 的非提交对象（`DESC-10j`）；其它命名空间（各自的 `DESC-10*` 卡）；`describe` 输出生成；现有查找对完整名字 `refs/heads/<n>` 的本地分支回退先于 tag 级的既有顺序差异（`DEFER-DESC-08`，见 `ADR-DESC-03` ①）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `refs/heads/` 名字在字面查找未命中时直接返回 `invalid_reference` | `heads/`/`refs/heads/` 早返回（`src/utils/util.rs:2444-2449`） |
| pinned Git 对任意前缀（含 `refs/heads/`）的 describe 名按同一规则解析、歧义时 commit 优先 | `ADR-DESC-03` |
| pinned Git 在解析 describe 名之前按 `ref_rev_parse_rules` 以完整名字查找其它命名空间的引用 | `ADR-DESC-03` Context（`refs.c:643-651`、`:827-835`；`object-name.c:1131`、`:1136`） |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse refs/heads/x-1-g<commit 的 8 位缩写>` 输出该 commit。
- [ ] AC2：`libra rev-parse refs/heads/x-1-g<commit 全长 OID>` 输出该 commit。
- [ ] AC3：存在同名字面引用 `refs/heads/blit-1-g<ab(B)>` 时解析为其目标而非后缀对象。
- [ ] AC4：`libra rev-parse refs/heads/x-1-g92ae10d0`（commit 与 blob 共享前缀）解析为该 commit。
- [ ] AC5：只存在以完整名字 `refs/heads/dw-1-g<ab(C)>` 命名的轻量 tag（指向 `P`）、本命名空间无同名引用时，`libra rev-parse refs/heads/dw-1-g<ab(C)>` 输出 `P`（跨命名空间完整名字查找，`ADR-DESC-03` ①「字面查找顺序」）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 与网站 `rev-parse.en.md`（说明 `refs/heads/` 前缀的 describe 名解析 commit），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC1–AC5 由 `(new)` `command::rev_parse_test::describe_suffix_ns_refs_heads_commit` 承载；夹具 `FX-DESC-RP`（含远程 `o`/`S`/`SC` 与歧义常量）；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1：stdout 恰为 `C`（现状以 `LBR-CLI-003` 失败）。
- AC2：stdout 恰为 `C`。
- AC3：`libra branch blit-1-g<ab(B)> <P>` 后，`libra rev-parse refs/heads/blit-1-g<ab(B)>` 的 stdout 恰为 `P`（先解析后缀的错误实现得到 `B` 或失败）。
- AC4：stdout 恰为 `92ae10d00618309f4ecba5b1735d693bc74ceaf5`。
- AC5：`libra tag refs/heads/dw-1-g<ab(C)> <P>`（轻量 tag，对应 Git `refs/tags/refs/heads/…`）后，`libra rev-parse refs/heads/dw-1-g<ab(C)>` 的 stdout 恰为 `P`（只查本命名空间即进入后缀 helper 的错误实现得到 `C`；现状以 `LBR-CLI-003` 失败；pinned Git 对同构夹具返回 `P`，见 `ADR-DESC-03` Context）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_ns_refs_heads_commit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-10c, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `refs/heads/` 前缀的 describe 名解析 commit（含字面引用优先、跨命名空间完整名字查找与 commit 优先消歧）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（9/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=refs/heads/ 命名空间后缀解析 commit; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-10c,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-10c; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-10e: `refs/remotes/` 命名空间 describe 后缀解析 commit

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（10/16）。在 `remotes/`/`refs/remotes/` 早返回分支（`src/utils/util.rs:2450-2455`）中，对以 `refs/remotes/` 开头的名字：本命名空间字面查找未命中且名字符合 describe 语法时，先按 `ADR-DESC-03` ①「字面查找顺序」以完整名字查找 tag、本地分支与远程跟踪引用，仍未命中才调用 describe 后缀 helper（含 `DESC-09b` 的缺失与 commit 优先消歧语义）解析 commit；字面引用优先。非提交对象由同家族的 `DESC-10k` 放开、其它前缀由同家族各自的卡接入（家族内实现顺序：中间态只存在于本地子卡提交、不对外发布，故不设临时边界判据，见 `REL-DESC-09`）。

**Out of scope:** `refs/remotes/` 的非提交对象（`DESC-10k`）；其它命名空间（各自的 `DESC-10*` 卡）；`describe` 输出生成。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `refs/remotes/` 名字在字面查找未命中时直接返回 `invalid_reference` | `remotes/`/`refs/remotes/` 早返回（`src/utils/util.rs:2450-2455`） |
| pinned Git 对任意前缀（含 `refs/remotes/`）的 describe 名按同一规则解析、歧义时 commit 优先 | `ADR-DESC-03` |
| pinned Git 在解析 describe 名之前按 `ref_rev_parse_rules` 以完整名字查找其它命名空间的引用 | `ADR-DESC-03` Context（`refs.c:643-651`、`:827-835`；`object-name.c:1131`、`:1136`） |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse refs/remotes/o/x-1-g<commit 的 8 位缩写>` 输出该 commit。
- [ ] AC2：`libra rev-parse refs/remotes/o/x-1-g<commit 全长 OID>` 输出该 commit。
- [ ] AC3：存在同名字面引用 `refs/remotes/o/rlit-1-g<ab(B)>` 时解析为其目标而非后缀对象。
- [ ] AC4：`libra rev-parse refs/remotes/o/x-1-g92ae10d0`（commit 与 blob 共享前缀）解析为该 commit。
- [ ] AC5：只存在以完整名字 `refs/remotes/o/dw-1-g<ab(C)>` 命名的本地分支（指向 `P`）、本命名空间无同名引用时，`libra rev-parse refs/remotes/o/dw-1-g<ab(C)>` 输出 `P`（跨命名空间完整名字查找，`ADR-DESC-03` ①「字面查找顺序」）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 与网站 `rev-parse.en.md`（说明 `refs/remotes/` 前缀的 describe 名解析 commit），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC1–AC5 由 `(new)` `command::rev_parse_test::describe_suffix_ns_refs_remotes_commit` 承载；夹具 `FX-DESC-RP`（含远程 `o`/`S`/`SC` 与歧义常量）；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1：stdout 恰为 `C`（现状以 `LBR-CLI-003` 失败）。
- AC2：stdout 恰为 `C`。
- AC3：在 `S` 中建分支 `rlit-1-g<ab(B)>` 指向 `SC` 后 `libra fetch o` 后，`libra rev-parse refs/remotes/o/rlit-1-g<ab(B)>` 的 stdout 恰为 `SC`（先解析后缀的错误实现得到 `B` 或失败）。
- AC4：stdout 恰为 `92ae10d00618309f4ecba5b1735d693bc74ceaf5`。
- AC5：`libra branch refs/remotes/o/dw-1-g<ab(C)> <P>`（本地分支，对应 Git `refs/heads/refs/remotes/o/…`；`S` 中无分支 `dw-1-g<ab(C)>`）后，`libra rev-parse refs/remotes/o/dw-1-g<ab(C)>` 的 stdout 恰为 `P`（只查本命名空间即进入后缀 helper 的错误实现得到 `C`；现状以 `LBR-CLI-003` 失败；pinned Git 对同构夹具返回 `P`，见 `ADR-DESC-03` Context）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_ns_refs_remotes_commit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-10d, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `refs/remotes/` 前缀的 describe 名解析 commit（含字面引用优先、跨命名空间完整名字查找与 commit 优先消歧）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（10/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=refs/remotes/ 命名空间后缀解析 commit; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-10d,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-10d; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-10f: `tags/` 命名空间 describe 后缀解析非提交对象（tree、blob、附注 tag）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（11/16）。为 `DESC-10` 已接入的 `tags/` describe 后缀放开 tree、blob 与附注 tag 对象（均返回对象自身，tag 不剥皮），至此 `tags/` 与 Git 的对象类型范围一致；两 blob 共享前缀仍按 `ADR-DESC-03` ② 以 `LBR-CLI-003` 失败。

**Out of scope:** 其它命名空间的非提交对象（各自的 `DESC-10f..10k` 卡）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DESC-10` 本地提交后 `tags/` 后缀只解析 commit（家族内中间态，不对外发布） | `REL-DESC-09`、`ADR-DESC-03` ③ |
| pinned Git 实测：`heads/x-1-g<8 位 tree>`、`nonsense-1-g<8 位 blob>`、`tags/q-3-g<8 位 tag 对象>`、`remotes/o/x-1-g<40 位 tag 对象>` 均返回对象自身；两 blob 共享前缀时失败 | `ADR-DESC-03` |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse tags/x-1-g<tree 的 8 位缩写>` 输出该 tree。
- [ ] AC2：`libra rev-parse tags/x-1-g<tree 全长 OID>` 输出该 tree。
- [ ] AC3：`libra rev-parse tags/x-1-g<blob 的 8 位缩写>` 输出该 blob。
- [ ] AC4：`libra rev-parse tags/x-1-g<blob 全长 OID>` 输出该 blob。
- [ ] AC5：`libra rev-parse tags/x-1-g<附注 tag 对象的 8 位缩写>` 输出该 tag 对象自身。
- [ ] AC6：`libra rev-parse tags/x-1-g<附注 tag 对象全长 OID>` 输出该 tag 对象自身。
- [ ] AC7：`libra rev-parse tags/x-1-gf6a3ecac`（两 blob 共享前缀）以 `LBR-CLI-003` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 与网站 `rev-parse.en.md`（补充 `tags/` 前缀可解析 tree、blob 与附注 tag 对象），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::rev_parse_test::describe_suffix_ns_tags_noncommit` 承载；夹具 `FX-DESC-RP` 的 `T`/`B`/`G`/`P` 与歧义常量；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1、AC2：stdout 恰为 `T`（各一次比较；未放行的现状以 `LBR-CLI-003` 失败）。
- AC3、AC4：stdout 恰为 `B`（各一次比较）。
- AC5、AC6：stdout 恰为 `G`（各一次比较；剥皮的错误实现输出 `P`）。
- AC7：错误码为 `LBR-CLI-003`（`parse_cli_error_stderr` 的 `error_code`；放开非提交对象后任取其一的错误消歧输出某个 blob）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_ns_tags_noncommit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-10e, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `tags/` 前缀的 describe 后缀可解析 tree、blob 与附注 tag 对象（不剥皮）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（11/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=tags/ 命名空间后缀放行非提交对象; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-10e,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-10e; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-10g: `heads/` 命名空间 describe 后缀解析非提交对象（tree、blob、附注 tag）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（12/16）。为 `DESC-10a` 已接入的 `heads/` describe 后缀放开 tree、blob 与附注 tag 对象（均返回对象自身，tag 不剥皮），至此 `heads/` 与 Git 的对象类型范围一致；两 blob 共享前缀仍按 `ADR-DESC-03` ② 以 `LBR-CLI-003` 失败。

**Out of scope:** 其它命名空间的非提交对象（各自的 `DESC-10f..10k` 卡）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DESC-10a` 本地提交后 `heads/` 后缀只解析 commit（家族内中间态，不对外发布） | `REL-DESC-09`、`ADR-DESC-03` ③ |
| pinned Git 实测：`heads/x-1-g<8 位 tree>`、`nonsense-1-g<8 位 blob>`、`tags/q-3-g<8 位 tag 对象>`、`remotes/o/x-1-g<40 位 tag 对象>` 均返回对象自身；两 blob 共享前缀时失败 | `ADR-DESC-03` |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse heads/x-1-g<tree 的 8 位缩写>` 输出该 tree。
- [ ] AC2：`libra rev-parse heads/x-1-g<tree 全长 OID>` 输出该 tree。
- [ ] AC3：`libra rev-parse heads/x-1-g<blob 的 8 位缩写>` 输出该 blob。
- [ ] AC4：`libra rev-parse heads/x-1-g<blob 全长 OID>` 输出该 blob。
- [ ] AC5：`libra rev-parse heads/x-1-g<附注 tag 对象的 8 位缩写>` 输出该 tag 对象自身。
- [ ] AC6：`libra rev-parse heads/x-1-g<附注 tag 对象全长 OID>` 输出该 tag 对象自身。
- [ ] AC7：`libra rev-parse heads/x-1-gf6a3ecac`（两 blob 共享前缀）以 `LBR-CLI-003` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 与网站 `rev-parse.en.md`（补充 `heads/` 前缀可解析 tree、blob 与附注 tag 对象），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::rev_parse_test::describe_suffix_ns_heads_noncommit` 承载；夹具 `FX-DESC-RP` 的 `T`/`B`/`G`/`P` 与歧义常量；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1、AC2：stdout 恰为 `T`（各一次比较；未放行的现状以 `LBR-CLI-003` 失败）。
- AC3、AC4：stdout 恰为 `B`（各一次比较）。
- AC5、AC6：stdout 恰为 `G`（各一次比较；剥皮的错误实现输出 `P`）。
- AC7：错误码为 `LBR-CLI-003`（`parse_cli_error_stderr` 的 `error_code`；放开非提交对象后任取其一的错误消歧输出某个 blob）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_ns_heads_noncommit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-10f, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `heads/` 前缀的 describe 后缀可解析 tree、blob 与附注 tag 对象（不剥皮）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（12/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=heads/ 命名空间后缀放行非提交对象; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-10f,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-10f; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-10h: `remotes/` 命名空间 describe 后缀解析非提交对象（tree、blob、附注 tag）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（13/16）。为 `DESC-10b` 已接入的 `remotes/` describe 后缀放开 tree、blob 与附注 tag 对象（均返回对象自身，tag 不剥皮），至此 `remotes/` 与 Git 的对象类型范围一致；两 blob 共享前缀仍按 `ADR-DESC-03` ② 以 `LBR-CLI-003` 失败。

**Out of scope:** 其它命名空间的非提交对象（各自的 `DESC-10f..10k` 卡）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DESC-10b` 本地提交后 `remotes/` 后缀只解析 commit（家族内中间态，不对外发布） | `REL-DESC-09`、`ADR-DESC-03` ③ |
| pinned Git 实测：`heads/x-1-g<8 位 tree>`、`nonsense-1-g<8 位 blob>`、`tags/q-3-g<8 位 tag 对象>`、`remotes/o/x-1-g<40 位 tag 对象>` 均返回对象自身；两 blob 共享前缀时失败 | `ADR-DESC-03` |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse remotes/o/x-1-g<tree 的 8 位缩写>` 输出该 tree。
- [ ] AC2：`libra rev-parse remotes/o/x-1-g<tree 全长 OID>` 输出该 tree。
- [ ] AC3：`libra rev-parse remotes/o/x-1-g<blob 的 8 位缩写>` 输出该 blob。
- [ ] AC4：`libra rev-parse remotes/o/x-1-g<blob 全长 OID>` 输出该 blob。
- [ ] AC5：`libra rev-parse remotes/o/x-1-g<附注 tag 对象的 8 位缩写>` 输出该 tag 对象自身。
- [ ] AC6：`libra rev-parse remotes/o/x-1-g<附注 tag 对象全长 OID>` 输出该 tag 对象自身。
- [ ] AC7：`libra rev-parse remotes/o/x-1-gf6a3ecac`（两 blob 共享前缀）以 `LBR-CLI-003` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 与网站 `rev-parse.en.md`（补充 `remotes/` 前缀可解析 tree、blob 与附注 tag 对象），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::rev_parse_test::describe_suffix_ns_remotes_noncommit` 承载；夹具 `FX-DESC-RP` 的 `T`/`B`/`G`/`P` 与歧义常量；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1、AC2：stdout 恰为 `T`（各一次比较；未放行的现状以 `LBR-CLI-003` 失败）。
- AC3、AC4：stdout 恰为 `B`（各一次比较）。
- AC5、AC6：stdout 恰为 `G`（各一次比较；剥皮的错误实现输出 `P`）。
- AC7：错误码为 `LBR-CLI-003`（`parse_cli_error_stderr` 的 `error_code`；放开非提交对象后任取其一的错误消歧输出某个 blob）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_ns_remotes_noncommit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-10g, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `remotes/` 前缀的 describe 后缀可解析 tree、blob 与附注 tag 对象（不剥皮）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（13/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=remotes/ 命名空间后缀放行非提交对象; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-10g,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-10g; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-10i: `refs/tags/` 命名空间 describe 后缀解析非提交对象（tree、blob、附注 tag）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（14/16）。为 `DESC-10c` 已接入的 `refs/tags/` describe 后缀放开 tree、blob 与附注 tag 对象（均返回对象自身，tag 不剥皮），至此 `refs/tags/` 与 Git 的对象类型范围一致；两 blob 共享前缀仍按 `ADR-DESC-03` ② 以 `LBR-CLI-003` 失败。

**Out of scope:** 其它命名空间的非提交对象（各自的 `DESC-10f..10k` 卡）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DESC-10c` 本地提交后 `refs/tags/` 后缀只解析 commit（家族内中间态，不对外发布） | `REL-DESC-09`、`ADR-DESC-03` ③ |
| pinned Git 实测：`heads/x-1-g<8 位 tree>`、`nonsense-1-g<8 位 blob>`、`tags/q-3-g<8 位 tag 对象>`、`remotes/o/x-1-g<40 位 tag 对象>` 均返回对象自身；两 blob 共享前缀时失败 | `ADR-DESC-03` |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse refs/tags/x-1-g<tree 的 8 位缩写>` 输出该 tree。
- [ ] AC2：`libra rev-parse refs/tags/x-1-g<tree 全长 OID>` 输出该 tree。
- [ ] AC3：`libra rev-parse refs/tags/x-1-g<blob 的 8 位缩写>` 输出该 blob。
- [ ] AC4：`libra rev-parse refs/tags/x-1-g<blob 全长 OID>` 输出该 blob。
- [ ] AC5：`libra rev-parse refs/tags/x-1-g<附注 tag 对象的 8 位缩写>` 输出该 tag 对象自身。
- [ ] AC6：`libra rev-parse refs/tags/x-1-g<附注 tag 对象全长 OID>` 输出该 tag 对象自身。
- [ ] AC7：`libra rev-parse refs/tags/x-1-gf6a3ecac`（两 blob 共享前缀）以 `LBR-CLI-003` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 与网站 `rev-parse.en.md`（补充 `refs/tags/` 前缀可解析 tree、blob 与附注 tag 对象），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::rev_parse_test::describe_suffix_ns_refs_tags_noncommit` 承载；夹具 `FX-DESC-RP` 的 `T`/`B`/`G`/`P` 与歧义常量；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1、AC2：stdout 恰为 `T`（各一次比较；未放行的现状以 `LBR-CLI-003` 失败）。
- AC3、AC4：stdout 恰为 `B`（各一次比较）。
- AC5、AC6：stdout 恰为 `G`（各一次比较；剥皮的错误实现输出 `P`）。
- AC7：错误码为 `LBR-CLI-003`（`parse_cli_error_stderr` 的 `error_code`；放开非提交对象后任取其一的错误消歧输出某个 blob）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_ns_refs_tags_noncommit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-10h, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `refs/tags/` 前缀的 describe 后缀可解析 tree、blob 与附注 tag 对象（不剥皮）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（14/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=refs/tags/ 命名空间后缀放行非提交对象; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-10h,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-10h; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-10j: `refs/heads/` 命名空间 describe 后缀解析非提交对象（tree、blob、附注 tag）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（15/16）。为 `DESC-10d` 已接入的 `refs/heads/` describe 后缀放开 tree、blob 与附注 tag 对象（均返回对象自身，tag 不剥皮），至此 `refs/heads/` 与 Git 的对象类型范围一致；两 blob 共享前缀仍按 `ADR-DESC-03` ② 以 `LBR-CLI-003` 失败。

**Out of scope:** 其它命名空间的非提交对象（各自的 `DESC-10f..10k` 卡）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DESC-10d` 本地提交后 `refs/heads/` 后缀只解析 commit（家族内中间态，不对外发布） | `REL-DESC-09`、`ADR-DESC-03` ③ |
| pinned Git 实测：`heads/x-1-g<8 位 tree>`、`nonsense-1-g<8 位 blob>`、`tags/q-3-g<8 位 tag 对象>`、`remotes/o/x-1-g<40 位 tag 对象>` 均返回对象自身；两 blob 共享前缀时失败 | `ADR-DESC-03` |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse refs/heads/x-1-g<tree 的 8 位缩写>` 输出该 tree。
- [ ] AC2：`libra rev-parse refs/heads/x-1-g<tree 全长 OID>` 输出该 tree。
- [ ] AC3：`libra rev-parse refs/heads/x-1-g<blob 的 8 位缩写>` 输出该 blob。
- [ ] AC4：`libra rev-parse refs/heads/x-1-g<blob 全长 OID>` 输出该 blob。
- [ ] AC5：`libra rev-parse refs/heads/x-1-g<附注 tag 对象的 8 位缩写>` 输出该 tag 对象自身。
- [ ] AC6：`libra rev-parse refs/heads/x-1-g<附注 tag 对象全长 OID>` 输出该 tag 对象自身。
- [ ] AC7：`libra rev-parse refs/heads/x-1-gf6a3ecac`（两 blob 共享前缀）以 `LBR-CLI-003` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 与网站 `rev-parse.en.md`（补充 `refs/heads/` 前缀可解析 tree、blob 与附注 tag 对象），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::rev_parse_test::describe_suffix_ns_refs_heads_noncommit` 承载；夹具 `FX-DESC-RP` 的 `T`/`B`/`G`/`P` 与歧义常量；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1、AC2：stdout 恰为 `T`（各一次比较；未放行的现状以 `LBR-CLI-003` 失败）。
- AC3、AC4：stdout 恰为 `B`（各一次比较）。
- AC5、AC6：stdout 恰为 `G`（各一次比较；剥皮的错误实现输出 `P`）。
- AC7：错误码为 `LBR-CLI-003`（`parse_cli_error_stderr` 的 `error_code`；放开非提交对象后任取其一的错误消歧输出某个 blob）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_ns_refs_heads_noncommit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-10i, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `refs/heads/` 前缀的 describe 后缀可解析 tree、blob 与附注 tag 对象（不剥皮）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（15/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=refs/heads/ 命名空间后缀放行非提交对象; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-10i,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-10i; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-10k: `refs/remotes/` 命名空间 describe 后缀解析非提交对象（tree、blob、附注 tag）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 自 `DESC-09` 拆出；家族 `REL-DESC-09` 子卡（16/16）。为 `DESC-10e` 已接入的 `refs/remotes/` describe 后缀放开 tree、blob 与附注 tag 对象（均返回对象自身，tag 不剥皮），至此 `refs/remotes/` 与 Git 的对象类型范围一致；两 blob 共享前缀仍按 `ADR-DESC-03` ② 以 `LBR-CLI-003` 失败。

**Out of scope:** 其它命名空间的非提交对象（各自的 `DESC-10f..10k` 卡）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DESC-10e` 本地提交后 `refs/remotes/` 后缀只解析 commit（家族内中间态，不对外发布） | `REL-DESC-09`、`ADR-DESC-03` ③ |
| pinned Git 实测：`heads/x-1-g<8 位 tree>`、`nonsense-1-g<8 位 blob>`、`tags/q-3-g<8 位 tag 对象>`、`remotes/o/x-1-g<40 位 tag 对象>` 均返回对象自身；两 blob 共享前缀时失败 | `ADR-DESC-03` |

**Acceptance criteria:**

- [ ] AC1：`libra rev-parse refs/remotes/o/x-1-g<tree 的 8 位缩写>` 输出该 tree。
- [ ] AC2：`libra rev-parse refs/remotes/o/x-1-g<tree 全长 OID>` 输出该 tree。
- [ ] AC3：`libra rev-parse refs/remotes/o/x-1-g<blob 的 8 位缩写>` 输出该 blob。
- [ ] AC4：`libra rev-parse refs/remotes/o/x-1-g<blob 全长 OID>` 输出该 blob。
- [ ] AC5：`libra rev-parse refs/remotes/o/x-1-g<附注 tag 对象的 8 位缩写>` 输出该 tag 对象自身。
- [ ] AC6：`libra rev-parse refs/remotes/o/x-1-g<附注 tag 对象全长 OID>` 输出该 tag 对象自身。
- [ ] AC7：`libra rev-parse refs/remotes/o/x-1-gf6a3ecac`（两 blob 共享前缀）以 `LBR-CLI-003` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 与网站 `rev-parse.en.md`（补充 `refs/remotes/` 前缀可解析 tree、blob 与附注 tag 对象），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::rev_parse_test::describe_suffix_ns_refs_remotes_noncommit` 承载；夹具 `FX-DESC-RP` 的 `T`/`B`/`G`/`P` 与歧义常量；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1、AC2：stdout 恰为 `T`（各一次比较；未放行的现状以 `LBR-CLI-003` 失败）。
- AC3、AC4：stdout 恰为 `B`（各一次比较）。
- AC5、AC6：stdout 恰为 `G`（各一次比较；剥皮的错误实现输出 `P`）。
- AC7：错误码为 `LBR-CLI-003`（`parse_cli_error_stderr` 的 `error_code`；放开非提交对象后任取其一的错误消歧输出某个 blob）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `rev-parse`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_ns_refs_remotes_noncommit)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`

**Full-suite trigger:** `N/A（family child：不执行 C 组；本地跑本卡 A 组 focused 门与 fmt、clippy，双环境全量由家族发布点 DESC-11 按 T-2 承担）` | **Dependencies:** `DESC-10j, DEP-DESC-UTIL, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/utils/util.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（family child） | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`（原地，不改行数），说明 `refs/remotes/` 前缀的 describe 后缀可解析 tree、blob 与附注 tag 对象（不剥皮）；EN 文档保留（`DESC-09` 新增）「尚未发布」说明句 `S_U`（`ADR-DESC-04`）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` 由本卡按「文档与网站验收命令」第 2–7 步同卡交付（`--only rev-parse`；`S_U` 见该节家族条目）。

**Rollback mode:** `compensating`（家族发布前按 `REL-DESC-09` 逆序以 `libra revert` 撤回本地子卡代码提交；已推送的本卡网站提交以一笔恢复改动前内容的新 `cf` 提交补偿，幂等键为本卡记录的网站提交 SHA 与改动前 SHA-256） | **Estimated scope:** `M` | **Version increment:** `N/A`（family child，不 bump） | **Release boundary:** `family child of REL-DESC-09（16/16）` | **C/D coverage from:** `DESC-11`

**Granularity:** `type=implementation; axis=refs/remotes/ 命名空间后缀放行非提交对象; recovery=compensating：逆序撤回本地子卡代码提交并以新 cf 提交补偿网站; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DESC-10j,DEP-DESC-UTIL,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-10j; release=family child; split-from=DESC-09; exception=N/A`

---

### Task DESC-11: describe 后缀家族 `REL-DESC-09` 发布点

**Task type:** `release` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1；`#798`/`#799` 往返前置的首个对外发布 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)、[#799](https://github.com/libra-tools/libra/issues/799)

**Description:** `REL-DESC-09` 的唯一发布点（G-08）。不引入新行为：在包含 `DESC-09`、`DESC-09b`、`DESC-09c`、`DESC-09a`、`DESC-10`、`DESC-10a`、`DESC-10b`、`DESC-10c`、`DESC-10d`、`DESC-10e`、`DESC-10f`、`DESC-10g`、`DESC-10h`、`DESC-10i`、`DESC-10j`、`DESC-10k` 十六张子卡全部本地签名提交的最终合树上，执行聚合守卫、发布说明本地核对与完整 C 组（`patch + 1`、版本面同步、工具链刷新 `Cargo.lock`、T-2 双环境全量 nextest、fmt、clippy、`cargo build --release`、签名+DCO 提交、PR 与合入核验、以 `--notes "$NOTES"` 的 `gh release create "v${VERSION}" --target "$MERGE_SHA" --title "v${VERSION}"`）及 D 组（含发布后以 `gh release view` 复核发布说明）；文档方面只做 `ADR-DESC-04` 规定的两步：ER-05 评审前删除 EN 文档中的 `S_U`（AC5），D 组全绿后同步网站并核验线上不再含 `S_U`（AC6）；不改变命令行为。

**Out of scope:** 任何新行为（由子卡承担）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 子卡在家族发布前只本地提交、不推送；C/D 覆盖由本卡提供 | 「发布分组与并发窗口」`REL-DESC-09`；模板 G-08、ER-04 |
| 子卡在 EN 文档与网站保留「尚未发布」句 `S_U`，由本卡在发布时移除 | `ADR-DESC-04`；「文档与网站验收命令」家族条目 |

**Acceptance criteria:**

- [ ] AC1：（B 组）ER-05 评审前，在只含十六张子卡提交、尚未 bump 的家族合树上，十六张子卡的卡特有用例（下列「聚合守卫·B 组」所列 16 条）在同一次 nextest 运行中全部通过。
- [ ] AC2：（C 组第 ④ 步）bump 与 `Cargo.lock` 刷新之后、提交之前，同一组 16 个用例在同一次 nextest 运行中全部通过。
- [ ] AC3：发布提交的内容与 AC2 的被测工作区内容逐文件一致。
- [ ] AC4：（B 组）ER-05 评审前准备的发布说明文本（只存于 shell 变量 `NOTES`，C 组原样作为 `gh release create --notes "$NOTES"` 的参数）包含固定句 `rev-parse now resolves git describe output names (ref-count-ghex), including names prefixed with tags/, heads/, remotes/, refs/tags/, refs/heads/ or refs/remotes/`。
- [ ] AC5：（B 组）ER-05 评审前，工作区 `docs/commands/rev-parse.md` 等于家族合树 `HEAD` 中该文件删去恰等于 `S_U` 的那一行后的内容。
- [ ] AC6：D 组全绿后，网站 `rev-parse.en.md` 经「文档与网站验收命令」第 2–6 步同步并推送 `cf`，线上页面以 `S_U` 为判定句的「页面检查」结果为不含。
- [ ] ER-06/ER-06a 必需文档同步：本卡不改变命令行为，不触发（`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217` 不变）；本卡特有的文档判据为 AC5、AC6（计入 G-03）。

**AC → 断言映射:**

- AC1 → 下列 Verification「聚合守卫·B 组」：在 bump 前的家族合树上运行，`cargo nextest list` 恰好列出 16 条，`N = P = 16`、`F = 0`；记录 `libra rev-parse HEAD`。
- AC2 → 下列 Verification「聚合守卫·C 组」：运行前记录 `libra rev-parse HEAD`（`H0`）、`libra status --porcelain` 中本卡改动的路径集合（须为版本面集合与 `docs/commands/rev-parse.md`，加上工具链刷新确有差异时的 `Cargo.lock`；出现其它路径即 STOP 并改在只含家族子卡提交的隔离 checkout 执行）及这些文件的工作区 SHA-256，`N = P = 16`、`F = 0`。
- AC3 → 下列 Verification「提交内容绑定」：提交后核对：`libra rev-parse 'HEAD^'` 等于 `H0`，`libra show --name-only --format=format: HEAD` 的非空行集合等于记录的路径集合，每个路径的 `libra show HEAD:<path> | shasum -a 256` 等于记录值——任一不符即须重跑本门；并记录发布提交的 `libra rev-parse 'HEAD^{tree}'`。
- AC4 → 下列 Verification「发布说明」：评审前对 `NOTES` 做一次 `rg -F -q` 判定（B 组本地门），C 组 `gh release create` 使用同一变量；Release 创建后以 `gh release view "v${VERSION}" --json body --jq .body` 复核同一固定句属 D 组后置核验。
- AC5 → 下列 Verification「文档标记·S_U」：`S_U` 取「文档与网站验收命令」家族条目的固定句；先 `if base="$(libra show HEAD:docs/commands/rev-parse.md)"; then rc=0; else rc=$?; fi`（`rc` 非 0 或 `base` 为空即 STOP），前置断言 `printf '%s\n' "$base" | grep -c -x -F -- "$S_U"` 输出 `1`（家族合树恰有一行 `S_U`）；判据：`if diff <(printf '%s\n' "$base" | grep -v -x -F -- "$S_U") docs/commands/rev-parse.md; then rc=0; else rc=$?; fi` 的 `rc` 为 0（1 = 内容不符即 FAIL，>1 判执行失败；基线只存于 shell 变量）。
- AC6 → 下列 Verification「网站删除核验」：按家族条目「发布点」② 执行；推送前「页面检查」（判定句 `S_U`）须为含（前置断言），推送后有界轮询中最后一次「页面检查」的 `hit` 为 1（不含），记录各次 `rc`、`hit`、UTC 时间与第 6 步的 `cf` 提交 SHA。

**Verification:**

- [ ] 聚合守卫·B 组（AC1）：评审前、bump 前在家族合树上运行 `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::describe_suffix_commit) or test(=command::rev_parse_test::describe_suffix_missing_and_ambiguous) or test(=command::rev_parse_test::describe_suffix_tag_object) or test(=command::rev_parse_test::describe_suffix_blob_and_tree) or test(=command::rev_parse_test::describe_suffix_ns_tags_commit) or test(=command::rev_parse_test::describe_suffix_ns_heads_commit) or test(=command::rev_parse_test::describe_suffix_ns_remotes_commit) or test(=command::rev_parse_test::describe_suffix_ns_refs_tags_commit) or test(=command::rev_parse_test::describe_suffix_ns_refs_heads_commit) or test(=command::rev_parse_test::describe_suffix_ns_refs_remotes_commit) or test(=command::rev_parse_test::describe_suffix_ns_tags_noncommit) or test(=command::rev_parse_test::describe_suffix_ns_heads_noncommit) or test(=command::rev_parse_test::describe_suffix_ns_remotes_noncommit) or test(=command::rev_parse_test::describe_suffix_ns_refs_tags_noncommit) or test(=command::rev_parse_test::describe_suffix_ns_refs_heads_noncommit) or test(=command::rev_parse_test::describe_suffix_ns_refs_remotes_noncommit)'`（`cargo nextest list` 须恰好列出这 16 条）
- [ ] 聚合守卫·C 组（AC2）：C 组第 ④ 步在 bump 与 `Cargo.lock` 刷新后重跑同一命令（运行前记录项见 AC2 映射）
- [ ] 提交内容绑定（AC3）：提交后执行 AC3 映射中的 `libra rev-parse 'HEAD^'`、`libra show --name-only --format=format: HEAD` 与逐文件 `libra show HEAD:<path> | shasum -a 256` 比较，全部一致才继续推送
- [ ] 文档标记·S_U（AC5）：评审前执行 AC5 映射中的 `diff` 判定，`rc` 须为 0
- [ ] 网站删除核验（AC6）：D 组全绿后按「文档与网站验收命令」家族条目「发布点」② 执行第 2–6 步与删除核验，最后一次「页面检查」`hit` 须为 1
- [ ] 发布说明（AC4）：`if rg -F -q -- 'rev-parse now resolves git describe output names (ref-count-ghex), including names prefixed with tags/, heads/, remotes/, refs/tags/, refs/heads/ or refs/remotes/' <<<"$NOTES"; then hit=0; else hit=$?; fi`，`hit` 须为 0（1 = FAIL，>1 判执行失败）；C 组 `gh release create "v${VERSION}" --target "$MERGE_SHA" --title "v${VERSION}" --notes "$NOTES"` 使用同一变量

**Full-suite trigger:** `T-2：家族发布点（C 组第 ④ 步在 bump 后跑双环境完整 nextest）` | **Dependencies:** `DESC-10k, DEP-DESC-UTIL, DEP-DESC-DOCS`

**Implementation write set:** `docs/commands/rev-parse.md`（只删除 `S_U` 一行）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`（D 组全绿后同步）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）（版本面与 `Cargo.lock` 属 Release write set；不改代码与测试）

**Release write set:** `Inherited`（版本面集合 + `Cargo.lock` + release artifact） | **Files likely touched:** 版本面集合、`Cargo.lock` 与 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** `docs/commands/rev-parse.md`：评审前删除 `S_U` 所在行（AC5）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`：D 组全绿后由 `python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --backend-root /Volumes/Data/gitmono/libra-backend --write --only rev-parse` 同步并核验线上不再含 `S_U`（AC6，`DEP-DESC-DOCS` 串行窗口）；`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md:217`：`N/A`（发布点不改变命令行为，`S_U` 只存在于 EN 文档，见 `ADR-DESC-04`）。

**Rollback mode:** `immutable-release`（发布后只能前滚新 patch，降级按 `PREV_VER`；发布前失败按 `REL-DESC-09`「失败恢复」分状态处理） | **Estimated scope:** `S` | **Version increment:** `patch` | **Release boundary:** `family release point of REL-DESC-09` | **C/D coverage from:** `self`

**Granularity:** `type=release; axis=REL-DESC-09 聚合发布; recovery=immutable-release：前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=6/12; VER=6/12; landing=0; prod-files=0; scope=S; deps=DESC-10k,DEP-DESC-UTIL,DEP-DESC-DOCS; writeset=序列化于 DESC-10k; release=family release point; split-from=N/A; exception=N/A`

---

### Task DESC-05: 支持 misnamed annotated tag 内部名称与 `--abbrev=0` 强制后缀输出（#799）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1；`#799` 修复与 `#798` 前置 | **Issue:** [#799](https://github.com/libra-tools/libra/issues/799)

**Description:** 非 `--contains` 路径中，附注 tag 的引用名（如 `refs/tags/Q`）与 tag 对象内部 `tag_name`（如 `A`）不一致时，按 Git `append_name()`（pinned `builtin/describe.c:297` 起）在 `TagInfo` 中记录内部名与 `misnamed = true`，仅对最终选定的名字向 stderr 打印 `warning: tag '<ref>' is externally known as '<tag_name>'`，并在格式输出中强制追加 `-<distance>-g<hash>`（`--abbrev=0` 时为完整 SHA，对齐 Git `repo_find_unique_abbrev(oid, 0)` 返回全长）；`--contains` 路径保持使用引用名。

**Out of scope:** `--all` 下的 misnamed 处理（Git 输出 `tags/<tag_name>` 并以 `tags/<ref>` 告警；本卡保持 `--all` 现有的引用名输出，登记 `DEFER-DESC-06`）；修改 `libra update-ref` 支持 `refs/tags/*`（本卡夹具改用 `libra tag`；上游 TC 依赖 `DEP-DESC-541`）；`--no-long`（`DESC-04`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `TagInfo` 仅存储 `t.name` 与 `is_annotated`，未使用 `TagObject::Tag(tg).tag_name` | `src/command/describe.rs:118-121, 225-258` |
| `describe_output` / `format_describe_result` 对 `abbrev == 0` 置 `abbreviated_commit: None` 且省略后缀 | `src/command/describe_format.rs:11-50` |
| `libra tag <name> <tag-oid>` 保留未剥皮的 tag 对象 | `src/command/tag.rs:817-869` |
| pinned Git（`2.56.0.116.g6de20f6092`）对同构夹具的实测结果 | `ADR-DESC-03` 后的 `FX-DESC-MN` |

**Acceptance criteria:**

- [ ] AC1：非 `--contains` 路径下 `libra describe <c>` 的 stdout 为以内部名开头的 `A-1-g<c 的 7 位缩写>`。
- [ ] AC2：同一命令 stderr 中含 `externally known as` 的行恰为一行 `warning: tag 'Q' is externally known as 'A'`（未被选中的 misnamed tag 不告警）。
- [ ] AC3：`libra describe --abbrev=0 <c>`（`t6120#32` 场景）的 stdout 为 `A-1-g<c 的完整 SHA>`。
- [ ] AC4：`libra describe --abbrev=0 --json <c>` 的 `data.tag` 为 `"A"`。
- [ ] AC5：同一命令的 `data.abbreviated_commit` 为 `c` 的完整 SHA。
- [ ] AC6：（`DEFER-DESC-06` 边界）`libra describe --all <c>` 的 stdout 为引用名形式 `tags/Q-1-g<c 的 7 位缩写>`。
- [ ] AC7：`libra describe --contains <Ca>` 的 stdout 为引用名形式 `Q^0`。
- [ ] AC8：同一 `--contains` 命令的 stderr 不含 `externally known as`。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219` 与网站 `describe.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::describe_test::misnamed_annotated_tag_uses_internal_name` 承载；夹具 `FX-DESC-MN`（`ADR-DESC-03` 后的共享夹具）；成功类命令须退出 0（执行条件））

- AC1：`libra describe <c>` 的 stdout 恰为 `A-1-g<c 的前 7 位>`（使用引用名的错误实现输出 `Q-1-g…`）。
- AC2：同一命令 stderr 中含子串 `externally known as` 的行组成的列表恰为 [`warning: tag 'Q' is externally known as 'A'`]（一次比较；不告警、重复告警或对未选中的 `R` 也告警的实现均不满足）。
- AC3：`libra describe --abbrev=0 <c>` 的 stdout 恰为 `A-1-g<c 的完整 SHA>`（按 `abbrev == 0` 省略后缀的现状输出 `A`）。
- AC4、AC5：`libra describe --abbrev=0 --json <c>` 的 `data.tag == "A"`、`data.abbreviated_commit == <c 的完整 SHA>`（各一次比较）。
- AC6：`libra describe --all <c>` 的 stdout 恰为 `tags/Q-1-g<c 的前 7 位>`（把 misnamed 处理意外扩到 `--all` 的实现输出 `tags/A-1-g…`；该差异由 `DEFER-DESC-06` 承接）。
- AC7：`libra describe --contains <Ca>` 的 stdout 恰为 `Q^0`。
- AC8：AC7 同一命令的 stderr 不含子串 `externally known as`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `describe`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::describe_test::misnamed_annotated_tag_uses_internal_name)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 非 nextest 门（TC-1045 / `t6120#32`）：`export TC_RUN_ID="desc-05-$(date -u +%Y%m%dT%H%M%SZ)-$$" TC_TMP_ROOT=/Volumes/Data/tmp/libra-testcases && cargo build --bin libra && LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1045-describe-abbrev/tc1045-describe-abbrev.sh`（判据与记录项按 `DEP-DESC-TC`；须在 `DEP-DESC-541` 满足后运行）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::describe_test::/) or test(/^command::describe_long_test::/)'`

**Full-suite trigger:** `T-1：本卡 patch + 1 修改 install.sh / install.ps1 版本面（C 组第 ④ 步在 bump 后跑双环境完整 nextest）` | **Dependencies:** `DESC-11, DEP-DESC-541, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC`

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_format.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md` 与 `COMPATIBILITY.md:219`（原地），说明 misnamed annotated tag 的内部名、警告与强制后缀（`--abbrev=0` 时完整 SHA）；网站页按「文档与网站验收命令」第 3 步以 `--only describe` 生成。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M`（修改公开 CLI 输出与警告） | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=misnamed tag 内部名与强制后缀; recovery=immutable-release：前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=2; scope=M; deps=DESC-11,DEP-DESC-541,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-11; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-04: 为 `libra describe` 增加 `--no-long` 并支持 last-wins 覆盖（#798）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1；Git CLI 选项兼容 | **Issue:** [#798](https://github.com/libra-tools/libra/issues/798)

**Description:** 在 `DescribeArgs` 中增加 `--no-long`，按 `ADR-DESC-01` 使 `--long` 与 `--no-long` 按命令行顺序 last-wins，并确认 misnamed tag（`DESC-05`）在 `--no-long` 下仍输出 `A-0-g<hash>` 且可由 `libra rev-parse` 解析回目标提交（`t6120#30`）。

**Out of scope:** `--no-abbrev`（`DESC-06`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DescribeArgs` 仅有 `#[clap(long)] pub long: bool` | `src/command/describe.rs:90-92` |
| `describe_commit_for_atom` 构造 `DescribeArgs` 字面量 | `src/command/describe.rs:159-173` |
| `long_format && abbrev == 0` 返回 `LongWithAbbrevZero` | `src/command/describe.rs:195-198` |
| pinned Git 实测：`--no-long --long` → `<tag>-<n>-g<hash>`；`--long --no-long --abbrev=0` → `<tag>`；misnamed `--no-long <Ca>` → `A-0-g<7 位>` | 本计划 R48 实测；`FX-DESC-MN` |

**Acceptance criteria:**

- [ ] AC1：`HEAD` 被附注 tag 直接指向时 `libra describe --no-long` 的 stdout 为 `<tag>`。
- [ ] AC2：`libra describe --long --no-long` 由 `--no-long` 生效，stdout 为 `<tag>`。
- [ ] AC3：`libra describe --long --no-long --json` 的 `data.long_format` 为 `false`。
- [ ] AC4：`libra describe --no-long --long` 由 `--long` 生效，stdout 为 `<tag>-0-g<hash>`。
- [ ] AC5：`libra describe --long --no-long --abbrev=0` 成功且 stdout 为 `<tag>`（不触发 `LongWithAbbrevZero`）。
- [ ] AC6：misnamed tag 上 `libra describe --no-long <A 所在提交>` 的 stdout 为 `A-0-g<hash>`。
- [ ] AC7：AC6 的输出可由 `libra rev-parse` 解析回该提交（`#798` / `t6120#30`）。
- [ ] AC8：（解析层）`DescribeArgs::try_parse_from(["describe", "--long", "--no-long"])` 的 `long` 为 `false`。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219` 与网站 `describe.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（成功类命令须退出 0（执行条件））

- AC1–AC5 → `(new)` `command::describe_long_test::test_describe_no_long_last_wins`（夹具：`create_committed_repo_via_cli()` 后 `libra tag -m v1.0 v1.0` 指向 `HEAD`；`ab7` 为 `HEAD` 前 7 位）：AC1 `libra describe --no-long` 的 stdout 恰为 `v1.0`；AC2 `libra describe --long --no-long` 恰为 `v1.0`；AC3 `libra describe --long --no-long --json` 的 `data.long_format == false`；AC4 `libra describe --no-long --long` 恰为 `v1.0-0-g<ab7>`；AC5 `libra describe --long --no-long --abbrev=0` 退出 0 且 stdout 恰为 `v1.0`（以 `--long` 出现过即报错的错误实现以 `LBR-CLI-002` 失败）。
- AC6、AC7 → `(new)` `command::describe_test::misnamed_annotated_tag_no_long_keeps_suffix`（夹具 `FX-DESC-MN`）：AC6 `libra describe --no-long <Ca>` 的 stdout 恰为 `A-0-g<Ca 的前 7 位>`；AC7 `libra rev-parse A-0-g<Ca 的前 7 位>` 的 stdout 恰为 `Ca` 的完整 SHA。
- AC8 → `(new)` 纯函数单元用例 `command::describe::tests::no_long_parse_order`：对该参数数组调用 `DescribeArgs::try_parse_from` 须返回 `Ok`，其 `long` 字段恰为 `false`（CLAUDE.md 要求新增选项配 focused 单元测试；不读写仓库、不改 cwd/env）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `describe`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::describe_long_test::test_describe_no_long_last_wins) or test(=command::describe_test::misnamed_annotated_tag_no_long_keeps_suffix)'`（均 `(new)`；`cargo nextest list` 须恰好列出这 2 条）
- [ ] 卡特有用例（AC8，解析层）：`source .env.test && cargo nextest run --lib -E 'test(=command::describe::tests::no_long_parse_order)'`（`(new)`；`cargo nextest list --lib` 须恰好列出该 1 条）
- [ ] 非 nextest 门（TC-1043 / `t6120#30`）：`export TC_RUN_ID="desc-04-$(date -u +%Y%m%dT%H%M%SZ)-$$" TC_TMP_ROOT=/Volumes/Data/tmp/libra-testcases && cargo build --bin libra && LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1043-describe-long/tc1043-describe-long.sh`（判据与记录项按 `DEP-DESC-TC`；须在 `DEP-DESC-541` 满足后运行）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::describe_test::/) or test(/^command::describe_long_test::/)'`；`source .env.test && cargo nextest run --lib -E 'test(/^command::describe::tests::/)'`

**Full-suite trigger:** `T-1：本卡 patch + 1 修改 install.sh / install.ps1 版本面（C 组第 ④ 步在 bump 后跑双环境完整 nextest）` | **Dependencies:** `DESC-05, DEP-DESC-541, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC`

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_tests.rs`, `tests/command/describe_long_test.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md` 与 `COMPATIBILITY.md:219`（原地），补充 `--no-long` 及 last-wins；网站页按「文档与网站验收命令」第 3 步以 `--only describe` 生成。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M`（新增 1 处公开 CLI 标志） | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--no-long 与 --long 的 last-wins; recovery=immutable-release：前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=1; scope=M; deps=DESC-05,DEP-DESC-541,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-05; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-06: 为 `libra describe` 增加 `--no-abbrev`（等价 `--abbrev=0`）（#800）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1；Git CLI 选项兼容 | **Issue:** [#800](https://github.com/libra-tools/libra/issues/800)

**Description:** 在 `DescribeArgs` 中增加 `--no-abbrev`（Git `OPT__ABBREV` 中等价于 `--abbrev=0`），与 `--abbrev=<N>` 按命令行顺序 last-wins：常规 tag 匹配时省略 `-<n>-g<hash>`（misnamed tag 仍按 `DESC-05` 强制后缀），`--always` 回退输出完整 commit SHA（含 tag 全部被 `--match` 过滤的场景，不再误报 `HEAD does not point to a commit`），`--long --no-abbrev` 按 `LongWithAbbrevZero` 拒绝。

**Out of scope:** `--dirty` 与 `<commit>` 的互斥校验（`DESC-07`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DescribeArgs` 仅有 `--abbrev <N>`，无 `--no-abbrev` | `src/command/describe.rs:74-76` |
| `long_format && abbrev == 0` 返回 `LongWithAbbrevZero`（`LBR-CLI-002`） | `src/command/describe.rs:195-198, 658-660` |
| `--always` 回退分支调用 `abbreviate_hash(&resolved_commit, abbrev)` | `src/command/describe.rs:362-378` |
| pinned Git 实测：`--no-abbrev --always --match=no-such-tag` → 40 位 SHA；`--abbrev=5 --no-abbrev` → `<tag>`；`--no-abbrev --abbrev=5` → `<tag>-<n>-g<5 位>`；`--long --no-abbrev` 失败（Git 不接受 `--abbrev 5` 空格形式，本卡夹具一律用 `--abbrev=5`） | 本计划 R48 实测 |

**Acceptance criteria:**

- [ ] AC1：存在 tag 但全部被过滤时，`libra describe --no-abbrev --always --match=no-such-tag`（`#800` / `t6120#110`）的 stdout 为 `HEAD` 的完整 SHA。
- [ ] AC2：`libra describe --no-abbrev --always --match=no-such-tag --json` 的 `data.used_always` 为 `true`。
- [ ] AC3：同一命令的 `data.abbreviated_commit` 等于 `HEAD` 的完整 SHA。
- [ ] AC4：常规可达 tag 存在时 `libra describe --no-abbrev` 的 stdout 为 `<tag>`。
- [ ] AC5：（解析层）按 `ADR-DESC-01` 以 `overrides_with` 实现时，`DescribeArgs::try_parse_from(["describe", "--abbrev=5", "--no-abbrev"])` 的（`abbrev`, `no_abbrev`）为（`None`, `true`），即后出现的 `--no-abbrev` 生效。
- [ ] AC6：`libra describe --no-abbrev --abbrev=5` 按 last-wins 输出 5 位缩写 `<tag>-<n>-g<5 位>`。
- [ ] AC7：`libra describe --long --no-abbrev` 以 `LBR-CLI-002` 失败。
- [ ] AC8：misnamed tag 上 `libra describe --no-abbrev <c>` 仍强制后缀并输出 `A-1-g<c 的完整 SHA>`（与 `--abbrev=0` 一致）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219` 与网站 `describe.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC1–AC4、AC6、AC7 由 `(new)` `command::describe_test::no_abbrev_last_wins_and_always` 承载，夹具：`create_committed_repo_via_cli()` 后写新文件 `n.txt` = `n1\n`、`libra add n.txt`、`libra commit -m c1 --no-verify` 得 `C1` 并 `libra tag -m v1.0 v1.0`，再写 `n.txt` = `n2\n`、`libra add n.txt`、`libra commit -m c2 --no-verify` 得 `C2`（`HEAD`）；前置断言：`libra rev-parse <C2 的前 5 位>` 恰输出 `C2`（5 位前缀在夹具中唯一，否则 STOP 并改用唯一长度后更新本卡）；AC8 由 `(new)` `command::describe_test::misnamed_annotated_tag_no_abbrev_keeps_full_suffix` 承载，夹具 `FX-DESC-MN`；成功类命令须退出 0、失败类命令须非 0 退出（执行条件））

- AC1：stdout 去尾换行后恰为 `libra rev-parse HEAD` 的输出（40 位；现状误报 `HEAD does not point to a commit` 或输出缩写的实现不满足）。
- AC2、AC3：`--json` 输出的 `data.used_always == true`；`data.abbreviated_commit` 是字符串且等于 `libra rev-parse HEAD` 的输出（字段缺失时为 `null`，不满足；各一次比较）。
- AC4：`libra describe --no-abbrev` 的 stdout 恰为 `v1.0`。
- AC6：`libra describe --no-abbrev --abbrev=5` 的 stdout 恰为 `v1.0-1-g<C2 的前 5 位>`。
- AC7：`libra describe --long --no-abbrev` 的错误码为 `LBR-CLI-002`（`parse_cli_error_stderr` 的 `error_code`）。
- AC8：`libra describe --no-abbrev <c>` 的 stdout 恰为 `A-1-g<c 的完整 SHA>`（`--no-abbrev` 一律省略后缀的错误实现输出 `A`）。
- AC5 → `(new)` 纯函数单元用例 `command::describe::tests::no_abbrev_parse_order`：`try_parse_from` 返回 `Ok`，二元组一次比较（CLAUDE.md 要求新增选项配 focused 单元测试；不读写仓库、不改 cwd/env）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `describe`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC4、AC6–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::describe_test::no_abbrev_last_wins_and_always) or test(=command::describe_test::misnamed_annotated_tag_no_abbrev_keeps_full_suffix)'`（均 `(new)`；`cargo nextest list` 须恰好列出这 2 条）
- [ ] 卡特有用例（AC5，解析层）：`source .env.test && cargo nextest run --lib -E 'test(=command::describe::tests::no_abbrev_parse_order)'`（`(new)`；`cargo nextest list --lib` 须恰好列出该 1 条）
- [ ] 非 nextest 门（TC-1046 / `t6120#110`）：`export TC_RUN_ID="desc-06-$(date -u +%Y%m%dT%H%M%SZ)-$$" TC_TMP_ROOT=/Volumes/Data/tmp/libra-testcases && cargo build --bin libra && LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1046-describe-abbrev/tc1046-describe-abbrev.sh`（判据与记录项按 `DEP-DESC-TC`）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::describe_test::/) or test(/^command::describe_long_test::/)'`；`source .env.test && cargo nextest run --lib -E 'test(/^command::describe::tests::/)'`

**Full-suite trigger:** `T-1：本卡 patch + 1 修改 install.sh / install.ps1 版本面（C 组第 ④ 步在 bump 后跑双环境完整 nextest）` | **Dependencies:** `DESC-04, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC`

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_format.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md` 与 `COMPATIBILITY.md:219`（原地），补充 `--no-abbrev`、last-wins 与 `--always` 完整 SHA；网站页按「文档与网站验收命令」第 3 步以 `--only describe` 生成。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M`（新增 1 处公开 CLI 标志） | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--no-abbrev 等价 --abbrev=0 的 last-wins; recovery=immutable-release：前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=DESC-04,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-04; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-07: `--dirty` 的适用范围：非 `--contains` 拒绝与 `<commit>` 共用，`--contains` 忽略（#802）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1；Git 参数校验缺陷 | **Issue:** [#802](https://github.com/libra-tools/libra/issues/802)

**Description:** 对齐 Git `describe.c` 的 `--dirty` 适用范围：非 `--contains` 路径同时给出 `--dirty` 与 `<commit>` 时 fail-closed（新增 `DescribeError::CommitishIncompatible { option }` 变体，映射 `LBR-CLI-002`；消息按 Git 写作 `option '<option>' and commit-ishes cannot be used together`、提示不再复用 `--match/--exclude` glob 文案——文案不设验收判据，失败类判据只看稳定错误码；`DESC-08` 复用该变体）；`--contains` 路径（含 `<commit>` 与默认 `HEAD`）接受 `--dirty`，其输出不受工作区状态影响（含脏检查会失败的状态）且不追加标记；实现方式为去掉 `src/command/describe.rs:427, 496` 的 `apply_dirty_mark` 调用，对齐 pinned `builtin/describe.c:665` 起的 `contains` 分支先行返回（实现指引，验收以 AC4–AC7 的可观测输出为准；GAP-DESC-01 后半）。

**Out of scope:** `--broken`（`DESC-08`）；`--contains` 的 `^0` 规则（已由 `DESC-01` 交付）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `args.commit` 与 `args.dirty` 未做互斥校验 | `src/command/describe.rs:62, 94-96, 188-209` |
| `InvalidArgument` 的提示固定为 `check the --match/--exclude glob syntax.` | `src/command/describe.rs:661-663`，`src/command/describe_types.rs:40-41` |
| `--contains` 路径调用 `apply_dirty_mark` | `src/command/describe.rs:427, 496` |
| pinned Git 实测：`--dirty HEAD` / `--dirty=-mk HEAD~1` → `fatal: option '--dirty' and commit-ishes cannot be used together`；`--contains --dirty [HEAD]` 在脏工作区与 index 被覆写为 `garbage\n` 时均输出 `<tag>^0` 且退出 0 | 本计划 R48/R49 实测 |

**Acceptance criteria:**

- [ ] AC1：`libra describe --dirty HEAD` 以 `LBR-CLI-002` 失败。
- [ ] AC2：（单元层）新增变体 `DescribeError::CommitishIncompatible { option: "--dirty".to_string() }` 的 `Display` 恰为 `option '--dirty' and commit-ishes cannot be used together`（按 `describe_error_display_pins_each_variant` 逐变体钉住文案的既有约定）。
- [ ] AC3：`libra describe --dirty HEAD --json` 的错误信封 `error_code` 为 `LBR-CLI-002`。
- [ ] AC4：脏工作区上 `libra describe --contains --dirty HEAD` 的 stdout 为 `<tag>^0`（接受且不追加标记）。
- [ ] AC5：脏工作区上 `libra describe --contains --dirty`（无 `<commit>`）的 stdout 为 `<tag>^0`。
- [ ] AC6：index 损坏（非 `--contains` 的脏检查会失败）时 `libra describe --contains --dirty HEAD` 的 stdout 为 `<tag>^0`（命令须退出 0，执行条件）。
- [ ] AC7：同一损坏状态下 `libra describe --contains --dirty`（无 `<commit>`）的 stdout 为 `<tag>^0`（命令须退出 0，执行条件）。
- [ ] AC8：脏工作区上 `libra describe --dirty`（无 `<commit>`）仍检查工作区并输出 `<tag>-dirty`。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219` 与网站 `describe.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（除 AC2 外均由 `(new)` `command::describe_test::dirty_scope_with_commit_and_contains` 承载。夹具：`create_committed_repo_via_cli()` 后写新文件 `f.txt` = `A\n`、`libra add f.txt`、`libra commit -m C1 --no-verify` 得 `C1` 并 `libra tag -m v1.0 v1.0`（`HEAD` = `C1`，`f.txt` 此后为已跟踪文件）；「脏工作区」= 把 `f.txt` 改写为 `B\n`；「index 损坏」= 恢复 `f.txt` 后把 `.libra/index` 覆写为字节 `garbage\n`（前置断言：同状态 `libra describe --dirty` 非 0 退出，证明脏检查确实失败）；成功类命令须退出 0、失败类命令须非 0 退出（执行条件）；错误码取 `parse_cli_error_stderr` 的 `error_code`）

- AC1：`libra describe --dirty HEAD` 的错误码为 `LBR-CLI-002`（现状静默执行并退出 0）。
- AC2 → 本卡修改的既有单元用例 `command::describe::tests::describe_error_display_pins_each_variant`：新增一条对该变体 `to_string()` 的 `assert_eq!`（纯函数，不读写仓库）。
- AC3：`--json` 错误信封的 `error_code == "LBR-CLI-002"`。
- AC4、AC5：脏工作区上两条命令的 stdout 均恰为 `v1.0^0`（各一次比较；现状输出 `v1.0^0-dirty`）。
- AC6、AC7：index 损坏状态下两条命令均退出 0 且 stdout 恰为 `v1.0^0`（各一次比较；把检查错误向上传播的现状与错误实现非 0 退出）。
- AC8：脏工作区上 `libra describe --dirty` 的 stdout 恰为 `v1.0-dirty`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `describe`）。

**Verification:**

- [ ] 卡特有用例（AC1、AC3–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::describe_test::dirty_scope_with_commit_and_contains)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 卡特有用例（AC2，单元层）：`source .env.test && cargo nextest run --lib -E 'test(=command::describe::tests::describe_error_display_pins_each_variant)'`（本卡修改的既有用例；`cargo nextest list --lib` 须恰好列出该 1 条）
- [ ] 非 nextest 门（TC-1052 / `t6120#42`）：`export TC_RUN_ID="desc-07-$(date -u +%Y%m%dT%H%M%SZ)-$$" TC_TMP_ROOT=/Volumes/Data/tmp/libra-testcases && cargo build --bin libra && LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1052-describe-dirty/tc1052-describe-dirty.sh`（判据与记录项按 `DEP-DESC-TC`）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::describe_test::/) or test(/^command::describe_long_test::/)'`；`source .env.test && cargo nextest run --lib -E 'test(/^command::describe::tests::/)'`

**Full-suite trigger:** `T-1：本卡 patch + 1 修改 install.sh / install.ps1 版本面（C 组第 ④ 步在 bump 后跑双环境完整 nextest）` | **Dependencies:** `DESC-06, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC`

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_types.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md` 与 `COMPATIBILITY.md:219`（原地），说明 `--dirty` 与 `<commit>` 不兼容（非 `--contains`）及 `--contains` 忽略 `--dirty`；网站页按「文档与网站验收命令」第 3 步以 `--only describe` 生成。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M`（修改公开 CLI 错误路径） | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--dirty 的适用范围; recovery=immutable-release：前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=DESC-06,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-06; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-08: 为 `libra describe` 实现 `--broken[=<mark>]` 的脏标记与适用范围（#803）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1；Git CLI 选项兼容 | **Issue:** [#803](https://github.com/libra-tools/libra/issues/803)

**Description:** 在 `DescribeArgs` 中增加 `--broken[=<mark>]`（默认 `-broken`），对齐 Git `describe.c`：脏检查先刷新 stat（仅 stat 变化、内容未变时不追加标记）；工作区真实脏时追加 `--dirty` 给出的标记（未给 `--dirty` 时默认 `-dirty`），后缀选择集中在新增纯函数 `worktree_suffix`；非 `--contains` 路径与 `<commit>` 共用时复用 `DESC-07` 新增的 `DescribeError::CommitishIncompatible` 变体（`LBR-CLI-002`；消息按 Git 写作 `option '--broken' and commit-ishes cannot be used together`，文案不设判据）；`--contains` 路径（含 `<commit>` 与默认 `HEAD`）接受 `--broken` 且输出不受工作区状态影响。脏检查本身失败时追加 `<mark>` 的行为按 G-09 拆出 `DESC-08a`：本卡期间该情形保持既有 `LBR-IO-001` 失败（临时边界）。

**Out of scope:** 脏检查失败时追加 `<mark>`（按 G-09 拆出 `DESC-08a`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DescribeArgs` 有 `--dirty[=<mark>]`，无 `--broken` | `src/command/describe.rs:94-96` |
| `has_tracked_dirty_changes` 将 status 错误统一转为 `DescribeError::ReadFailure` | `src/command/describe.rs:548-561` |
| pinned Git 实测：`--broken` 在脏工作区 → `<tag>-dirty`；`--dirty=-dm --broken` → `<tag>-dm`；index 被覆写为 `garbage` 后 `--broken` → `<tag>-broken`、`--broken=-bk` → `<tag>-bk`（退出 0），同状态 `--dirty` 失败；`test-tool chmtime` 仅改 stat 后 `--dirty --broken` → `<tag>`；`--broken HEAD` → `fatal: option '--broken' and commit-ishes cannot be used together`；`--contains --broken HEAD` → `<tag>^0` | 本计划 R48 实测；`t6120-describe.sh:776-792`（pinned 上游缓存） |

**Acceptance criteria:**

- [ ] AC1：仅 stat 变化（内容未变）时 `libra describe --dirty --broken` 的 stdout 为 `<tag>`（`#803` / `t6120#109`）。
- [ ] AC2：工作区真实脏时 `libra describe --broken`（无 `--dirty`）的 stdout 为 `<tag>-dirty`。
- [ ] AC3：工作区真实脏时 `libra describe --dirty=-dm --broken` 的 stdout 为 `<tag>-dm`。
- [ ] AC4：`libra describe --broken HEAD`（非 `--contains`）以 `LBR-CLI-002` 失败。
- [ ] AC5：index 损坏时 `libra describe --contains --broken HEAD` 的 stdout 为 `<tag>^0`（命令须退出 0，执行条件）。
- [ ] AC6：index 损坏时 `libra describe --contains --broken`（无 `<commit>`）的 stdout 为 `<tag>^0`（命令须退出 0，执行条件）。
- [ ] AC7：（单元层）本卡新增的纯函数 `worktree_suffix(dirty_mark: Option<&str>, check: WorktreeCheck) -> Option<String>`（或等价签名；`WorktreeCheck` 为本卡在 `src/command/describe_types.rs` 新增的枚举，取 `Clean`/`Dirty`）在 `dirty_mark = None`、`check = Dirty` 时返回 `Some("-dirty")`。
- [ ] AC8：（临时边界）index 损坏时非 `--contains` 的 `libra describe --broken` 以 `LBR-IO-001` 失败（检查失败时追加标记由 `DESC-08a` 交付）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219` 与网站 `describe.en.md`（说明检查失败时的标记尚未支持），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC1–AC6 由 `(new)` `command::describe_test::broken_marks_and_scope` 承载；AC8 由 `(new)` 边界用例 `command::describe_test::broken_check_failure_boundary` 承载，`DESC-08a` 删除该用例。夹具：`create_committed_repo_via_cli()` 后写新文件 `f.txt` = `A\n`、`libra add f.txt`、`libra commit -m A --no-verify` 并 `libra tag -m v1.0 v1.0`（`f.txt` 此后为已跟踪文件）；每个场景前把 `f.txt` 与 `.libra/index` 恢复为已提交状态；「脏工作区」= `f.txt` 改写为 `B\n`；「index 损坏」= `.libra/index` 覆写为字节 `garbage\n`（前置断言：同状态 `libra describe --dirty` 非 0 退出；若 ER-02 复现发现该方式不能令脏检查失败，改用能令其失败的最小 index 损坏并记录，前置断言不变）；成功类命令须退出 0、失败类命令须非 0 退出（执行条件）；错误码取 `parse_cli_error_stderr` 的 `error_code`）

- AC1：以 `std::fs::File::set_modified` 把 `f.txt` 的 mtime 改为原值减 10 秒（对齐上游 `test-tool chmtime -10`；前置断言：`f.txt` 字节仍等于 `A\n` 且 mtime 已变化），`libra describe --dirty --broken` 的 stdout 恰为 `v1.0`（把 stat 变化当作脏的错误实现输出 `v1.0-dirty`）。
- AC2：脏工作区上 `libra describe --broken` 的 stdout 恰为 `v1.0-dirty`（缺省标记接线错误的实现输出 `v1.0` 或其它后缀）。
- AC3：脏工作区上 `libra describe --dirty=-dm --broken` 的 stdout 恰为 `v1.0-dm`（忽略显式 `--dirty` 标记的实现输出 `v1.0-dirty`）。
- AC4：`libra describe --broken HEAD` 的错误码为 `LBR-CLI-002`。
- AC5、AC6：index 损坏状态下 `libra describe --contains --broken HEAD` 与 `libra describe --contains --broken` 的 stdout 均恰为 `v1.0^0`（各一次比较；对 `--contains` 也执行脏检查或把缺省 `HEAD` 当作显式 commit 的错误实现失败）。
- AC7 → `(new)` 纯函数单元用例 `command::describe::tests::worktree_suffix_defaults_dirty_mark`：一次 `assert_eq!`（不读写仓库、不改 cwd/env；CLAUDE.md 要求新增选项配 focused 单元测试）。
- AC8：index 损坏状态下 `libra describe --broken` 的错误码为 `LBR-IO-001`（提前追加 `-broken` 的实现输出成功）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `describe`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC6、AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::describe_test::broken_marks_and_scope) or test(=command::describe_test::broken_check_failure_boundary)'`（均 `(new)`；`cargo nextest list` 须恰好列出这 2 条）
- [ ] 卡特有用例（AC7，单元层）：`source .env.test && cargo nextest run --lib -E 'test(=command::describe::tests::worktree_suffix_defaults_dirty_mark)'`（`(new)`；`cargo nextest list --lib` 须恰好列出该 1 条）
- [ ] 非 nextest 门（TC-1054 / `t6120#109`）：`export TC_RUN_ID="desc-08-$(date -u +%Y%m%dT%H%M%SZ)-$$" TC_TMP_ROOT=/Volumes/Data/tmp/libra-testcases && cargo build --bin libra && LIBRA_BIN="$PWD/target/debug/libra" bash ../libra-testcases/cases/tc1054-describe-dirty/tc1054-describe-dirty.sh`（判据与记录项按 `DEP-DESC-TC`）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::describe_test::/) or test(/^command::describe_long_test::/)'`；`source .env.test && cargo nextest run --lib -E 'test(/^command::describe::tests::/)'`

**Full-suite trigger:** `T-1：本卡 patch + 1 修改 install.sh / install.ps1 版本面（C 组第 ④ 步在 bump 后跑双环境完整 nextest）` | **Dependencies:** `DESC-07, DEP-DESC-DOCS, DEP-DESC-COMPAT, DEP-DESC-TC`

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_types.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md` 与 `COMPATIBILITY.md:219`（原地），补充 `--broken[=<mark>]`、stat 刷新、默认 `-dirty` 与 `<commit>` 不兼容；网站页按「文档与网站验收命令」第 3 步以 `--only describe` 生成。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M`（新增 1 处公开 CLI 标志） | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--broken 的脏标记与适用范围; recovery=immutable-release：前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=DESC-07,DEP-DESC-DOCS,DEP-DESC-COMPAT,DEP-DESC-TC; writeset=序列化于 DESC-07; release=independent; split-from=N/A; exception=N/A`

---

### Task DESC-08a: `--broken` 在脏检查失败时追加标记（#803）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空` | **Priority / Selection:** P1；Git CLI 选项兼容 | **Issue:** [#803](https://github.com/libra-tools/libra/issues/803)

**Description:** 自 `DESC-08` 拆出（G-09）。脏检查本身失败（如 index 损坏）时，`--broken` 不再以 `LBR-IO-001` 失败，而是追加 `--broken` 给出的标记（默认 `-broken`），并与 `--dirty` 一样经 `apply_dirty_mark` 写入 `result` 与 `dirty_mark`（`ADR-DESC-02`）；`worktree_suffix` 增加 `broken_mark` 参数与 `WorktreeCheck::Failed` 分支（`describe_types.rs`）。删除 `DESC-08` 的临时边界用例 `command::describe_test::broken_check_failure_boundary`。

**Out of scope:** `--broken` 的脏标记与适用范围（已由 `DESC-08` 交付）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `has_tracked_dirty_changes` 将 status 错误统一转为 `DescribeError::ReadFailure`（`LBR-IO-001`） | `src/command/describe.rs:548-561`、`:634-636` |
| pinned Git 实测：index 被覆写为 `garbage\n` 后 `--broken` → `<tag>-broken`、`--broken=-bk` → `<tag>-bk`（退出 0） | 本计划 R48 实测 |

**Acceptance criteria:**

- [ ] AC1：index 损坏时 `libra describe --broken` 的 stdout 为 `<tag>-broken`。
- [ ] AC2：index 损坏时 `libra describe --broken=-bk` 的 stdout 为 `<tag>-bk`。
- [ ] AC3：index 损坏时 `libra describe --broken --json` 的 `data.dirty_mark` 为 `"-broken"`。
- [ ] AC4：index 损坏时同一 `libra describe --broken --json` 的 `data.result` 为 `"<tag>-broken"`。
- [ ] AC5：（单元层）`worktree_suffix` 签名扩展为 `worktree_suffix(dirty_mark: Option<&str>, broken_mark: Option<&str>, check: WorktreeCheck)`（或等价）、`WorktreeCheck` 增加 `Failed` 后，在 `check = Failed`、`broken_mark = Some("-bk")` 时返回 `Some("-bk")`。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219` 与网站 `describe.en.md`（移除「检查失败时的标记尚未支持」的说明），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC1–AC4 由 `(new)` `command::describe_test::broken_check_failure_marks` 承载。夹具：`create_committed_repo_via_cli()` 后写新文件 `f.txt`（内容 `A\n`）、`libra add f.txt`、`libra commit -m A --no-verify`，再 `libra tag -m v1.0 v1.0`（附注，指向该提交）；随后把 `.libra/index` 覆写为字节 `garbage\n`（「index 损坏」；本卡三条命令都在该状态下运行，均只读、无需在命令间恢复）。前置断言：同状态 `libra describe --dirty` 非 0 退出；若 ER-02 复现发现该方式不能令脏检查失败，改用能令其失败的最小 index 损坏并记录，前置断言不变。成功类命令须退出 0（执行条件））

- AC1、AC2：stdout 分别恰为 `v1.0-broken`、`v1.0-bk`（各一次比较；仍以 `LBR-IO-001` 失败的现状或忽略自定义标记的实现不满足）。
- AC3：`--json` 输出的 `data.dirty_mark == "-broken"`（只在 `result` 追加标记而不写 `dirty_mark` 的实现不满足）。
- AC4：同一 `--json` 输出的 `data.result == "v1.0-broken"`（只写 `dirty_mark` 而不在 JSON `result` 追加标记的实现不满足；`result` 与 `dirty_mark` 为 `DescribeOutput` 的独立字段，`src/command/describe_types.rs:5-18`）。
- AC5 → `(new)` 纯函数单元用例 `command::describe::tests::worktree_suffix_failed_uses_broken_mark`：一次 `assert_eq!`（不读写仓库、不改 cwd/env）。签名扩展时同步更新 `DESC-08` 单元用例 `worktree_suffix_defaults_dirty_mark` 的调用实参（`broken_mark` 传 `None`），其断言不变。
- 本卡删除 `DESC-08` 的边界用例 `command::describe_test::broken_check_failure_boundary`（其断言与本卡 AC1 相反）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步（`<cmd>` = `describe`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC4）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::describe_test::broken_check_failure_marks)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 卡特有用例（AC5，单元层）：`source .env.test && cargo nextest run --lib -E 'test(=command::describe::tests::worktree_suffix_failed_uses_broken_mark)'`（`(new)`；`cargo nextest list --lib` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::describe_test::/) or test(/^command::describe_long_test::/)'`；`source .env.test && cargo nextest run --lib -E 'test(/^command::describe::tests::/)'`

**Full-suite trigger:** `T-1：本卡 patch + 1 修改 install.sh / install.ps1 版本面（C 组第 ④ 步在 bump 后跑双环境完整 nextest）` | **Dependencies:** `DESC-08, DEP-DESC-DOCS, DEP-DESC-COMPAT`

**Implementation write set:** `src/command/describe.rs`, `src/command/describe_types.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** 同 `Implementation write set` | **Deliverables:** `Inherited`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md` 与 `COMPATIBILITY.md:219`（原地），说明脏检查失败时 `--broken` 追加 `<mark>`（默认 `-broken`）；网站页按「文档与网站验收命令」第 3 步以 `--only describe` 生成。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--broken 脏检查失败时追加标记; recovery=immutable-release：前滚新 patch / 降级 PREV_VER; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=1; prod-files=2; scope=M; deps=DESC-08,DEP-DESC-DOCS,DEP-DESC-COMPAT; writeset=序列化于 DESC-08; release=independent; split-from=DESC-08; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `command::describe::tests::describe_error_display_pins_each_variant`、`command::describe::tests::no_abbrev_parse_order`、`command::describe::tests::no_long_parse_order`、`command::describe::tests::no_match_parse_order`、`command::describe::tests::worktree_suffix_defaults_dirty_mark`、`command::describe::tests::worktree_suffix_failed_uses_broken_mark`（本计划新增或修改的纯函数单测，见「验证执行协议」第 6 条）；既有 `command::describe::tests` | 卡特有用例按各卡精确过滤；共享回归组 `source .env.test && cargo nextest run --lib -E 'test(/^command::describe::tests::/)'` |
| 集成（describe） | `command::describe_long_test::test_describe_no_long_last_wins`、`command::describe_test::all_candidates_respect_match_and_exclude`、`command::describe_test::broken_check_failure_marks`、`command::describe_test::broken_marks_and_scope`、`command::describe_test::contains_exact_match_flag_appends_caret_zero`、`command::describe_test::contains_exact_tag_appends_caret_zero`、`command::describe_test::contains_merge_parent_not_exact`、`command::describe_test::dirty_scope_with_commit_and_contains`、`command::describe_test::misnamed_annotated_tag_no_abbrev_keeps_full_suffix`、`command::describe_test::misnamed_annotated_tag_no_long_keeps_suffix`、`command::describe_test::misnamed_annotated_tag_uses_internal_name`、`command::describe_test::no_abbrev_last_wins_and_always`、`command::describe_test::no_match_resets_previous_match_patterns`、`command::describe_test::test_describe_contains_exact_match_requires_direct_tag`、`command::describe_test::test_describe_contains_names_relative_to_descendant_tag` | 卡特有用例按各卡精确过滤；共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::describe_test::/) or test(/^command::describe_long_test::/)'` |
| 集成（rev-parse） | `command::rev_parse_test::describe_suffix_blob_and_tree`、`command::rev_parse_test::describe_suffix_commit`、`command::rev_parse_test::describe_suffix_missing_and_ambiguous`、`command::rev_parse_test::describe_suffix_ns_heads_commit`、`command::rev_parse_test::describe_suffix_ns_heads_noncommit`、`command::rev_parse_test::describe_suffix_ns_refs_heads_commit`、`command::rev_parse_test::describe_suffix_ns_refs_heads_noncommit`、`command::rev_parse_test::describe_suffix_ns_refs_remotes_commit`、`command::rev_parse_test::describe_suffix_ns_refs_remotes_noncommit`、`command::rev_parse_test::describe_suffix_ns_refs_tags_commit`、`command::rev_parse_test::describe_suffix_ns_refs_tags_noncommit`、`command::rev_parse_test::describe_suffix_ns_remotes_commit`、`command::rev_parse_test::describe_suffix_ns_remotes_noncommit`、`command::rev_parse_test::describe_suffix_ns_tags_commit`、`command::rev_parse_test::describe_suffix_ns_tags_noncommit`、`command::rev_parse_test::describe_suffix_tag_object` | 卡特有用例按各卡精确过滤；共享回归组 `source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'` |
| 阶段性（非最终覆盖） | `command::describe_test::broken_check_failure_boundary`：`DESC-08` 的临时边界用例，只在 `DESC-08` 至 `DESC-08a` 之间存在，由 `DESC-08a` 删除，不属最终树必须覆盖的测试；最终覆盖由 `command::describe_test::broken_check_failure_marks` 承担 | `DESC-08` 的卡特有 Verification |
| 兼容 | `compat_version_surface_sync`、`COMPATIBILITY.md:217/219`、`DEP-DESC-TC`（`tc1031`、`tc1034`、`tc1035`、`tc1043`、`tc1045`、`tc1046`、`tc1052`、`tc1054`，上游 `t6120-describe.sh` @ `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`） | `source .env.test && cargo nextest run --test compat_version_surface_sync`（C 组版本面）及各卡「非 nextest 门」 |
| 迁移 | N/A（无 SQLite schema 迁移） | N/A |
| 安全 | `--dirty`/`--broken` 与 `<commit>` 共用 fail-closed（`LBR-CLI-002`）；describe 后缀缺失/歧义以 `LBR-CLI-003` fail-closed，且歧义时仅「恰一个 commit 候选」放行 | `DESC-07`、`DESC-08`、`DESC-09b` 及各命名空间卡的卡特有用例 |
| 性能 | 不引入额外全仓遍历，沿用现有 BFS/Dijkstra 与前缀检索 | 上述共享回归组 |
| live/gated | N/A（纯本地命令，无远端网络依赖；本地远程夹具为同机路径） | N/A |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| DESC-01 | [#795](https://github.com/libra-tools/libra/issues/795) (`src/command/describe.rs:412-496`, `describe_test.rs:555, 687`) | `src/command/describe.rs`, `tests/command/describe_test.rs` | `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219`、网站 `describe.en.md` | `command::describe_test::contains_exact_tag_appends_caret_zero`、`command::describe_test::contains_merge_parent_not_exact`、`command::describe_test::test_describe_contains_names_relative_to_descendant_tag` |
| DESC-01a | [#795](https://github.com/libra-tools/libra/issues/795) (`src/command/describe.rs:207, 411-427`, `describe_test.rs:687`) | `src/command/describe.rs`, `tests/command/describe_test.rs` | `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219`、网站 `describe.en.md` | `command::describe_test::contains_exact_match_flag_appends_caret_zero`、`command::describe_test::test_describe_contains_exact_match_requires_direct_tag` |
| DESC-02 | [#796](https://github.com/libra-tools/libra/issues/796) (`src/command/describe.rs:102-104`) | `src/command/describe.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs` | `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219`、网站 `describe.en.md` | `command::describe_test::no_match_resets_previous_match_patterns`、`command::describe::tests::no_match_parse_order` |
| DESC-03 | [#797](https://github.com/libra-tools/libra/issues/797) (`src/command/describe.rs:265-289`) | `src/command/describe.rs`, `tests/command/describe_test.rs` | `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219`、网站 `describe.en.md` | `command::describe_test::all_candidates_respect_match_and_exclude` |
| DESC-09 | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_commit` |
| DESC-09b | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_missing_and_ambiguous` |
| DESC-09c | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_tag_object` |
| DESC-09a | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_blob_and_tree` |
| DESC-10 | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_ns_tags_commit` |
| DESC-10a | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_ns_heads_commit` |
| DESC-10b | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_ns_remotes_commit` |
| DESC-10c | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_ns_refs_tags_commit` |
| DESC-10d | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_ns_refs_heads_commit` |
| DESC-10e | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_ns_refs_remotes_commit` |
| DESC-10f | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_ns_tags_noncommit` |
| DESC-10g | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_ns_heads_noncommit` |
| DESC-10h | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_ns_remotes_noncommit` |
| DESC-10i | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_ns_refs_tags_noncommit` |
| DESC-10j | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_ns_refs_heads_noncommit` |
| DESC-10k | #798/#799 往返前置 (`src/utils/util.rs:2404-2468`；`ADR-DESC-03`) | `src/utils/util.rs`, `tests/command/rev_parse_test.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md:217`、网站 `rev-parse.en.md` | `command::rev_parse_test::describe_suffix_ns_refs_remotes_noncommit` |
| DESC-11 | `REL-DESC-09` 发布点（#798/#799 往返前置；聚合 `DESC-09`、`DESC-09b`、`DESC-09c`、`DESC-09a` 与 `DESC-10..10k`） | `无（发布点）` | 评审前删除 `docs/commands/rev-parse.md` 中的 `S_U`；D 组全绿后同步网站 `rev-parse.en.md` 并核验线上不再含 `S_U`（`ADR-DESC-04`） | `command::rev_parse_test::describe_suffix_commit`、`command::rev_parse_test::describe_suffix_missing_and_ambiguous`、`command::rev_parse_test::describe_suffix_tag_object`、`command::rev_parse_test::describe_suffix_blob_and_tree`、`command::rev_parse_test::describe_suffix_ns_tags_commit`、`command::rev_parse_test::describe_suffix_ns_heads_commit`、`command::rev_parse_test::describe_suffix_ns_remotes_commit`、`command::rev_parse_test::describe_suffix_ns_refs_tags_commit`、`command::rev_parse_test::describe_suffix_ns_refs_heads_commit`、`command::rev_parse_test::describe_suffix_ns_refs_remotes_commit`、`command::rev_parse_test::describe_suffix_ns_tags_noncommit`、`command::rev_parse_test::describe_suffix_ns_heads_noncommit`、`command::rev_parse_test::describe_suffix_ns_remotes_noncommit`、`command::rev_parse_test::describe_suffix_ns_refs_tags_noncommit`、`command::rev_parse_test::describe_suffix_ns_refs_heads_noncommit`、`command::rev_parse_test::describe_suffix_ns_refs_remotes_noncommit` |
| DESC-05 | [#799](https://github.com/libra-tools/libra/issues/799) (`src/command/describe.rs:225-258`, `describe_format.rs:11-50`) | `src/command/describe.rs`, `src/command/describe_format.rs`, `tests/command/describe_test.rs` | `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219`、网站 `describe.en.md` | `command::describe_test::misnamed_annotated_tag_uses_internal_name` |
| DESC-04 | [#798](https://github.com/libra-tools/libra/issues/798) (`src/command/describe.rs:90-92`) | `src/command/describe.rs`, `src/command/describe_tests.rs`, `tests/command/describe_long_test.rs`, `tests/command/describe_test.rs` | `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219`、网站 `describe.en.md` | `command::describe_long_test::test_describe_no_long_last_wins`、`command::describe_test::misnamed_annotated_tag_no_long_keeps_suffix`、`command::describe::tests::no_long_parse_order` |
| DESC-06 | [#800](https://github.com/libra-tools/libra/issues/800) (`src/command/describe.rs:74-80, 362-378`) | `src/command/describe.rs`, `src/command/describe_format.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs` | `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219`、网站 `describe.en.md` | `command::describe_test::no_abbrev_last_wins_and_always`、`command::describe_test::misnamed_annotated_tag_no_abbrev_keeps_full_suffix`、`command::describe::tests::no_abbrev_parse_order` |
| DESC-07 | [#802](https://github.com/libra-tools/libra/issues/802)、#795 后半 (`src/command/describe.rs:62, 94-96, 427, 496`) | `src/command/describe.rs`, `src/command/describe_types.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs` | `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219`、网站 `describe.en.md` | `command::describe_test::dirty_scope_with_commit_and_contains`、`command::describe::tests::describe_error_display_pins_each_variant` |
| DESC-08 | [#803](https://github.com/libra-tools/libra/issues/803) (`src/command/describe.rs:94-96, 548-561`) | `src/command/describe.rs`, `src/command/describe_types.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs` | `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219`、网站 `describe.en.md` | `command::describe_test::broken_marks_and_scope`、`command::describe_test::broken_check_failure_boundary`（阶段性：由 `DESC-08a` 删除，最终覆盖为 `command::describe_test::broken_check_failure_marks`）、`command::describe::tests::worktree_suffix_defaults_dirty_mark` |
| DESC-08a | [#803](https://github.com/libra-tools/libra/issues/803) (`src/command/describe.rs:548-561`) | `src/command/describe.rs`, `src/command/describe_types.rs`, `src/command/describe_tests.rs`, `tests/command/describe_test.rs` | `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`COMPATIBILITY.md:219`、网站 `describe.en.md` | `command::describe_test::broken_check_failure_marks`、`command::describe::tests::worktree_suffix_failed_uses_broken_mark` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划文本经 Claude 与 Codex 对同一字节双审 `PASS`，基线与 `DEP-DESC-DOCS` 核对完成 | 本计划文档进入主线 | `libra revert` 文档提交 |
| M1 | `DESC-01`、`DESC-01a`、`DESC-02`、`DESC-03`、`DESC-09`、`DESC-09b`、`DESC-09c`、`DESC-09a`、`DESC-10`、`DESC-10a..10k`、`DESC-11`、`DESC-05`、`DESC-04`、`DESC-06` 全部 `done / complete`（`DESC-05`/`DESC-04` 在 `#541` 合入且各自 `DEP-DESC-TC` 输出精确 `TC-<id> PASS` 之后才进入 C/D） | 每张会发布卡（家族 `REL-DESC-09` 由 `DESC-11` 统一）`patch + 1`、`gh release create`、TC 记录项与 D 组全绿 | 前滚修复新 patch；已发布可按 `PREV_VER` 降级 |
| M2 | `DESC-07`、`DESC-08`、`DESC-08a` 全部 `done / complete`（含对应 TC）且全量收口门全绿 | 每卡独立发布、TC 记录 + 计划级全量 nextest 全绿 | 前滚修复新 patch；已发布可按 `PREV_VER` 降级 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 单卡本地实现或 focused 测试失败 | 未提交工作区改动 | 先核对 `libra status` / `libra diff` 差异归属：仅含本卡改动时执行 `libra restore --staged --worktree -- <card files>` 并清理本卡新增未跟踪文件；混有同文件无关改动时停止并做选择性恢复 | 带着失败测试提交或整文件覆盖无关改动 |
| PR checks 或 `release.yml` 失败 | 已合并提交 | 按 ER-04/ER-10 前滚新建修复卡/新 patch，必要时指引用户降级至 `PREV_VER` | 回退已推送提交或已发布 tag |
| 网站前置核验失败（非 `cf`、`origin` 不符、目标文件有他人改动、暂存区非空、本地分叉）或部署核验超预算 | 无后端写入，或已推送但未出现在线上 | 按「文档与网站验收命令」第 2/7 步 STOP 并报告协调者；部署未出现时本卡 `blocked` | 在非 `cf` 分支写入、丢弃无关改动、混入他人提交或强推 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `libra update-ref` 不支持 `refs/tags/*`（`src/command/update_ref.rs:6-10,147-150`） | 高：`TC-1045` 阻塞 `DESC-05`，`TC-1043` 阻塞 `DESC-04` | `DESC-05` 只等 `#541`+`TC-1045`；`DESC-04` 等 `DESC-05` 完成后再跑 `TC-1043`；未过各自 TC 不进入 C/D | `DESC-05`, `DESC-04` |
| `TC-1046` 可能受前序 `t6120` 状态级联影响 | 中：非 `--no-abbrev` 本身缺陷导致 `TC-1046` 失败 | `DESC-06` ER-02 复现时核实是否属前序级联，若是则登记前置阻塞缺陷，本卡保持 `blocked` | `DESC-06` |
| `src/utils/util.rs`、`tests/command/rev_parse_test.rs` 与 `rev-parse` 文档和同批计划写集重叠 | 中：I–I 写集碰撞 | `DEP-DESC-UTIL` 开工前核验并刷新行号 | `DESC-09`、`DESC-09a..09c`、`DESC-11`、`DESC-10`、`DESC-10a..10k` |
| describe 后缀歧义语义与 Libra 纯十六进制歧义不同（Git 对恰一个 commit 候选放行） | 中：直接复用纯十六进制歧义报错会拒绝 Git 接受的名字 | `ADR-DESC-03` ② 固定 Git 语义；`DESC-09b` 与各命名空间卡以 commit 优先消歧与 `LBR-CLI-003` 断言判定（提示文案不设判据） | `DESC-09b`、`DESC-10..10k` |
| `COMPATIBILITY.md` 增删行导致 `tests/compat-ledger` 行号断言失效 | 中：compat-ledger 门失败 | 所有卡仅原地修改 `:217`/`:219` 单行，`wc -l` 前后相等 | 全部卡 |
| 家族 `REL-DESC-09` 子卡的 `rev-parse` 网站页先于 `DESC-11` 发布公开（子卡按 ER-06a 同卡交付网站，代码在发布点才进入 `main`） | 中：窗口期内网站描述的行为尚未随版本可用 | 子卡在 EN 文档与网站加入「尚未发布」句 `S_U`，`DESC-11` 评审前从 EN 文档删除、D 组全绿后才同步网站并核验移除（`ADR-DESC-04`；`gh release create` 到网站同步完成之间网站偏保守地低报）；家族十六张子卡与 `DESC-11` 连续执行、不插入其它发布；家族失败按 `REL-DESC-09`「失败恢复」分状态处理 | `DESC-09`、`DESC-09b`、`DESC-09c`、`DESC-09a`、`DESC-10`、`DESC-10a..10k`、`DESC-11` |
| 网站生成产物（`docs-manifest.ts`、`docs-search.ts`、`dist/`）被同批计划并发改写 | 中：构建结果互相覆盖 | `DEP-DESC-DOCS` 串行窗口；不提交生成产物 | 全部卡 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| `describe` 祖先 BFS / `--contains` Dijkstra | $O(V + E)$ 可达提交图 | 无跨命令持久化状态 | 不引入额外全仓对象扫描 | `command::describe_test` 共享回归组 |
| `--match` / `--exclude` glob 匹配 | $O(R \cdot P)$（$R$ 为 ref 数，$P$ 为模式数，单模式 $\le 256$ B） | 单次命令内 | `MAX_GLOB_LEN = 256` | `command::describe::tests` 共享回归组 |
| describe 后缀解析 | 字面查找未命中后一次前缀检索（与纯十六进制回退同一 `search_result`）；歧义时读候选对象类型 | 单次命令内 | 不新增全量对象遍历；候选类型只在歧义分支读取 | `command::rev_parse_test` 共享回归组 |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已同步（`:217` `rev-parse`、`:219` `describe` 原地更新，行数不变）。
- [ ] `docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md` 已同步。
- [ ] `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md` 已同步。
- [ ] `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/{describe,rev-parse}.en.md` 已同步并核验部署；`docs/commands/rev-parse.md` 与网站 `rev-parse.en.md` 均已不含 `S_U`（`DESC-11` AC5、AC6，`ADR-DESC-04`）。
- [ ] `docs/error-codes.md` 已核对（沿用已有 `LBR-CLI-002`、`LBR-CLI-003`、`LBR-REPO-003`、`LBR-IO-001`，不新增稳定错误码）。
- [ ] `tests/INDEX.md` 已核对（复用 `command_test` 下的 `describe_test`、`describe_long_test`、`rev_parse_test`，无新增 target）。
- [ ] `Cargo.toml` `[[test]]` 已核对（无需新增 compat target）；`tests/SERIAL_REGISTRY.tsv` 未改动（新增用例不加 `#[serial]`，见「验证执行协议」第 6 条）。
- [ ] `plan-long.md` 日期计划索引已同步或说明 `N/A`。

## Codex review log

（自 R48 起本表同时记录 Claude 与 Codex 双审；表名沿用历史名称。）

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
| R25（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | CANCELLED | — | Codex R25b 已指出 `../libra-backend` 与 `/Volumes/Data/gitmono/libra-backend` 路径确认门，取消后合并至 R26 复审 | 只读评审（`2026-11-17 05:28:00 UTC`） |
| R25（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1-1（`../libra-backend` 与 `AGENTS.md` `/Volumes/Data/gitmono/libra-backend` 路径未显式要求开工前确认） | 已在 `DEP-DESC-DOCS` 增加 `/Volumes/Data/gitmono/libra-backend` 缺失时向协调者确认门，同时保留 `plan-template.md:650` `../libra-backend` 写法 | 只读评审（`2026-11-17 05:27:05 UTC`） |
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
| R32（Claude `claude-opus-5-5`） | `plan-20261117-describe.md` | 未执行 | — | — | 原登记为 Pending；未执行，被 R35 起的后续轮次取代（R48 订正） |
| R32（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | 未执行 | — | — | 原登记为 Pending；未执行，被 R35 起的后续轮次取代（R48 订正） |
| R35（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | 无新增 P1；仅完成局部检查，不能作为完整复审 | R36 完整复审，不以局部检查推定其它维度通过 | 只读评审（部分覆盖） |
| R36（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | P1: Phase 0 仍双审、网站缺独立 Claude Code PASS；P2: 文头修订时间陈旧。已改 Phase 0 为 Codex 单审、恢复网站独立 Claude Code PASS 并同步时间 | R37 完整复审 | Codex R36 只读评审（完整覆盖） |
| R37（Codex `gpt-6.1-terra`） | `plan-20261117-describe.md` | FAIL | 无新增 P1；未完成全部卡片与源码锚点复审，不能作为完整结论。已统一现行网站路径并保留历史记录 | R38 完整复审 | Codex R37 只读评审（部分覆盖） |
| R38-A（Codex `gpt-6.1-terra`） | 第 1–598 行，部分核对 | FAIL | P1：DESC-09 错误拒绝非提交对象；Git 可解析指向 blob 的表达式。已改为 blob/tree/commit/tag 与 Git 对照；本次未完成该区间全部合同核对 | R39-A 完整分区复审 | Codex R38-A 只读评审（部分覆盖） |
| R39-A（Codex `gpt-6.1-terra`） | 第 1–598 行及跨界依赖 | FAIL | P1：DESC-10 仍限制提交类型且漏完整 refs 命名空间。已覆盖六种前缀及 blob/tree/commit/tag、全长/缩写与字面 ref 优先回归 | R40-A 完整分区复审 | Codex R39-A 只读评审（分区覆盖） |
| R40-A（Codex `gpt-6.1-terra`） | 第 1–600 行及相关跨界卡（部分验证） | FAIL | P1：DESC-09/10 合并多个独立判据、实际超过 G-03 上限；非法后缀与字面引用优先冲突。已限定语法检查仅在字面引用未命中时执行，拆分与全区复审待完成 | 已拆分 `DESC-09a`、`DESC-10a`；重新完整分区复审 | Codex R40-A 只读评审（未完成全部合同核验） |
| R41-A（Codex `gpt-6.1-terra`） | 第 1–650 行及 DESC-09a/10a 完整卡 | FAIL | P1：DESC-09 字面引用优先及 DESC-10/10a 不同对象长度组合仍合并计数；新卡文档字段未逐文件列举。已补文档字段，计数与发布切片重拆待完成 | 完成拆分后复审 | Codex R41-A 只读评审（分区覆盖） |
| R47-A（Codex `gpt-6.1-terra`） | DESC-09 系列及 DESC-10/10a 定向检查 | FAIL | P1：集中单测矩阵漏新增测试；P2：顺序摘要漏 DESC-09c；其余指定范围因输出截断未完成 | 已补矩阵和顺序，须完整复审 | Codex R47-A 只读部分审查 |
| R48（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `485eb8fee7d4da427d5c6efb61a71f747a5d2f0276c02835055f56264cf465bc` | FAIL | P1：失败类 AC 断言 stderr 文案与本计划协议矛盾；`DESC-09`/`DESC-10..10e` 临时边界未验收；`DESC-05` 的 `--all` 与「仅选中告警」未验收 | P2：评审结论表两行未同步；「纯只读」未核实 index 刷新写入（登记为残余风险） | R49 修订（见修订历史 2026-11-18 15:31:27 UTC 行） |
| R48（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL | A：`is_valid_refname` 拒绝单层名、失败类文案判据、`COMPATIBILITY.md`/网站无关改动保护只比哈希；B：同 `is_valid_refname`、stdin 夹具与 `run_libra_command` 冲突、`DESC-03` AC7「与改动前相同」、`DESC-09b` 文案判据；C：文案判据、commit 阶段边界未验收；D：`DESC-05` AC1 与映射不同谓词、`--all`/告警范围未验收、`DESC-06` 缺 misnamed `--no-abbrev`、`DESC-07`/`08` 文案判据、`DESC-07` 不能证明 `--contains` 不做工作区检查 | 无 | R49 修订 |
| R49（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `94787a1e45ad33eb14e2178ad605c4b251c1dec92137fafa714adb3a0449ffce` | PASS | 无 P0/P1（事后被同字节 Codex 发现的 P1 推翻，见下行） | P2：夹具字面引用示例名、`DESC-07` 证据、协议第 1 条模块名 | 已随 R50 修订 |
| R49（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL | A：单独 `@` 的 ref 校验、TC 运行标识/缓存根未导出、网站无关改动快照未覆盖未跟踪目录与删除项、`COMPATIBILITY.md` 基线读取退出码未捕获；B：单独 `@`；C：边界只测 tree、「不修改既有测试」无法验证；D：`DESC-05` `--all` 告警未验收、损坏 index 不能证明未执行检查 | 无 | R50 修订 |
| R50（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `19484fd347b094649227037228308928195d95873e5868d88ed31d4978fb8443` | FAIL | P1：`ADR-DESC-03` ① 未排除单独 `@` 且仍称 `refs/` 前置即等价 | P2：`DESC-07`/`08` 退出码写法不一致、`DESC-09d` 未说明大小写 | 已随 R51 修订 |
| R50（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（D 分区 PASS） | A：ADR 单独 `@`、网站无变更分支与完成门冲突；B：`DESC-09d` 未验非十六进制计数/后缀、`DESC-09d` 未接线违反 G-02、`DESC-03` AC6「保持」缺快照；C：夹具 `'C^{tree}'` 字面量无法构造 | D：`DESC-06` AC3 可能 `null == null`、夹具未写全内容与 5 位前缀唯一性（已采纳） | R51 修订 |
| R51（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `66a7cf4ad25874d540325d25daf49b699c0cc511ecb1c8e959e386794b8d99da` | PASS | 无 P0/P1（事后被同字节 Codex 的 P1 推翻） | P2：`DESC-11` 多列 `DEP-DESC-COMPAT` | 已随 R52 修订 |
| R51（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（C 分区 PASS） | A：家族子卡网站拆到发布点违反 ER-06a、无关改动快照缺权限元数据、测试矩阵列入将删除的边界用例、`DESC-11` 树绑定时机；B：`DESC-02` 未测前置 `--exclude`、`DESC-09` 缺无字面引用的非法 `<hex>` 判据、`DESC-11` 发布说明判据弱且少计；D：`DESC-06/07/08` 夹具未 `libra add`、`DESC-05` AC6「保持」 | C：`SC` 未显式赋值（已采纳） | R52 修订 |
| R52（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `5830528355f7c40982d5d49d9cbae6b4a3e33d4eaf3a6415a3425e3e50b42e26` | FAIL | P1：`DESC-11` 提交后 `status` 为空在共享 checkout 不可满足 | P2：`DESC-11` 文档 AC 步号不一致 | 已随 R53 修订 |
| R52（Codex `gpt-6.1-terra`） | 同一受审 SHA-256 | 未运行 | — | — | 调用路径含 `..`，`codex exec -o` 拒绝打开输出文件，四分区均即时退出；不作结论 |
| R53（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `cbec1dde9e52249b9387ec0d559385ab8d8ef7a95562b3d5d3443cd1d2eed9e0` | PASS | 无 P0/P1（事后被同字节 Codex A/B 的 P1 推翻） | P2：`DESC-01` 映射说明文字 | 已随 R54 修订 |
| R53（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（C、D 分区 PASS） | A：家族网站后移不适用于非 `code-only`、`DESC-11` 测试与提交树绑定不足、发布说明 AC 与 B/C 组时序循环；B：`DESC-02` 未测多个前置 `--match`、`DESC-03` 未测 `--contains --all --exclude`、glob `*` 跨 `/` 语义未覆盖 | 无 | R54 修订 |
| R54（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `6ca6827e4ebdf8ac9c2e912a3be1836b47a71101f5f7528ecffe50af6e29c32d` | PASS | 无（事后被同字节 Codex A/B 的 P1 推翻） | 无 | 已随 R55 修订 |
| R54（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（C、D 分区 PASS） | A/B：追溯表仍把子卡网站交付归 `DESC-11`；B：`DESC-11` 聚合守卫只在 C 组运行、违反 B→评审→C 顺序；子卡网站先于版本公开会误导现有版本用户 | A：`Cargo.lock` 可能无差异（已采纳） | R55 修订 |
| R55（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `06b551a47e219ffdcba053fdc44a86b613e4272501b517dbfbf0d7cc070e34f6` | FAIL | P1：`DESC-11` AC3 的 B/C 组标注矛盾、AC4/AC5 依赖 bump 后版本号与评审先后冲突 | 无 | 已随 R56 修订 |
| R55（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（C 分区 PASS） | A：`DESC-11` VER 少计、标记验收与 ER-04 顺序循环、`S_R` 可能先于版本发布上线；B：同上两项（含失败回滚未覆盖标记）；D：`DESC-04/06/08` 新增选项缺 focused 单元测试 | 无 | R56 修订 |
| R56（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `5c38dd4f4deefe558d4058cf16d38cc841765f6a60027cf3502e2a9ae794c2d3` | PASS | 无（事后被同字节 Codex A/B/D 的 P1 推翻） | 无 | 已随 R57 修订 |
| R56（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（C 分区 PASS） | A/B：发布前恢复未覆盖 `DESC-11` 标记改动、C 组失败恢复未区分已推送/已合入、仓库 `main` 文档先于 Release 宣称已支持；D：解析层单测与所映射 AC 非同一谓词、`DESC-07` 新错误变体缺 `Display` 钉住测试 | 无 | R57 修订 |
| R57（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `4dd75f93ef782a61ba6b94dc22c9a42527ddb3793349e33ffe53f91ef9711507` | FAIL | P1：`DESC-11` AC2 合并两个谓词 | 无 | 已随 R58 修订 |
| R57（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（C 分区 PASS） | A：恢复状态①漏 `DESC-11` 版本提交、状态③④重叠、最终代码树门与记录提交自触发重跑；B：发布说明声称范围超过根命名空间；D：`DESC-08` 自定义 `--broken` 标记无端到端判据 | 无 | R58 修订 |
| R58（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `7bafd14bcb868f1c7bf8e63a9c0635dc83c72dd2a87007b53d55a84e6be0a9b2` | PASS | 无（事后被同字节 Codex A/D 的 P1 推翻） | P2：`DESC-08` `--contains` 脏工作区场景只由损坏 index 覆盖 | 已随 R59 记录 |
| R58（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（B、C 分区 PASS） | A：家族中途失败时恢复步骤要求撤销尚不存在的提交；D：`DESC-08` 缺 `--broken` 的 JSON 契约验收 | 无 | R59 修订 |
| R59（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `ba57aa55a8e5af692eb562afbbf4b53a8fb0f8aceccfe2b707e1d9f17d241af7` | PASS | 无 | P2：`DESC-08` 显式 `--dirty` 标记组合无判据（同字节 Codex D 判为 P1，已采纳） | 已随 R60 修订 |
| R59（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（B、C 分区 PASS） | A：家族恢复漏网站未推送残留、最终代码树判定漏未提交改动；D：`DESC-08` `--dirty=<dmark> --broken` 无验收 | 无 | R60 修订 |
| R60（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `3b4f2c93bed3f22ada5341e8df94a26c1fa8459c4008d52bcefbb08144650b6b` | PASS | 无（事后被同字节 Codex A/D 的 P1 推翻） | 无 | 已随 R61 修订 |
| R60（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（B、C 分区 PASS） | A：家族早期失败时基线可能不存在、已暂存网站改动的恢复只核对工作区；D：`DESC-08` 默认 `-dirty` 无端到端判据、`--contains --broken` 缺默认 `HEAD` 判据 | 无 | R61 修订（拆出 `DESC-08a`） |
| R61（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `50246c5a944e720372c56e5e6b8d37f44b48a94980a2ad2814a969705307402e` | FAIL | P1：测试矩阵把将被删除的 `broken_check_failure_boundary` 列为最终覆盖；P2：可靠性行未提 `DESC-08a`；P2（实测佐证）：index 损坏时 `--contains` 仍退出 0 | 无 | R62 修订 |
| R61（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（四分区均 FAIL） | A/D：测试矩阵与 `DESC-08a` 删除边界用例冲突；B：`generation == 0` 会把合并第二父误判为精确匹配；C：命名空间 commit-only 边界未覆盖全长非提交 OID、未接入前缀无临时边界；D：`FX-DESC-MN` 未写全提交构造、`DESC-08a` 夹具引用他卡 | 无 | R62 修订（并入家族、拆出 `DESC-01a`） |
| R62（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `012ff693841f304b0421a07f625a65f8ab8ea4ee429712dd6133bebd5e5501e7` | PASS | 无（事后被同字节 Codex A/B/C 的 P1 推翻） | P2 登记为残余风险：家族窗口扩至 16 张子卡、执行与 rebase 重验成本上升；`DESC-01` 与 `DESC-01a` 之间一个版本的 `--contains` 输出 `<tag>^0` 而 `--contains --exact-match` 仍为 `<tag>`（该版本文档已说明） | R63 修订 |
| R62（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（D 分区 PASS） | A：`DESC-01a` 改变公开 CLI 输出却标 `S`；B：家族子卡先行发布网站而 `S_A` 不说明尚未发布；C：命名空间接线未对齐 Git 跨命名空间字面查找（DWIM），分支 `tags/x-1-g<ab(B)>` 指向 `P` 时计划实现返回 `B`、`AC3` 测不出 | A：追溯表未标阶段性用例（R63 已注明）；D：`DESC-08a` 未直接断言 JSON `data.result`（同一 `DescribeOutput` 间接保障，登记残余风险） | R63 修订（新增 `ADR-DESC-04`，定案依据见该 ADR） |
| R63（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `f394c0fd54a84ba2e36529085fa1d0781fdd5107369569a5bf0c783ae0301807` | FAIL | P1：跨命名空间完整名字查找的远程跟踪级无判据覆盖（只查 tag 与本地分支的错误实现可通过） | P2：兼容与文档收口未核 `S_U` 移除（R64 已补） | R64 修订 |
| R63（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | FAIL（A、B、D 分区 PASS） | C：`refs/heads/` 现有查找的完整名字分支回退先于 Git 的 `refs/tags/%s`，`DESC-10d` AC5 测不出（R64 按既有差异如实描述并登记 `DEFER-DESC-08`，见 `ADR-DESC-03` ①） | D：`DESC-08a` 未直接断言 JSON `data.result`（R64 新增 AC4） | R64 修订 |
| R64（Claude `claude-opus-5-5` subagent） | 全文，受审 SHA-256 `527cab92cdf024ed3e5117589b13d28bad345621b6544c307b14c4233aeb343f` | FAIL | P1：`DEFER-DESC-08` 原因列「现状 fail-closed、不返回错误对象」与同行 `refs/heads/<n>` 顺序差异矛盾 | 无 | R65 修订 |
| R64（Codex `gpt-6.1-terra`，A/B/C/D 四分区并行） | 同一受审 SHA-256 | PASS（四分区均 PASS） | 无 | A、C：同上 `DEFER-DESC-08` 原因列矛盾（与 Claude P1 同一问题，R65 已改）；D：`DESC-08a` 未断言 JSON `dirty` 字段（登记残余风险，不改卡） | R65 差异确认 |
| R65（Claude `claude-opus-5-5` subagent，差异确认轮） | 相对 R64 字节的差异（第 5 行、修订历史一行、review log 两行、`DEFER-DESC-08` 行），受审 SHA-256 `10e7257d1dda82573adc468232ac6355dce9f7476166f02d1398c851a62b890d` | PASS | 无（R64 唯一 P1 已闭合） | 无 | 可提交 |
| R65（Codex `gpt-6.1-terra`，差异确认轮） | 同一受审 SHA-256（基线 R64 字节经 Codex 四分区全文 PASS） | PASS | 无 | P2：`DEFER-DESC-08` 原因列「非 describe 语法名字现状以 `LBR-CLI-003` 失败」未排除 `refs/heads/<n>` 完整名字分支回退命中的情形（登记残余风险） | 可提交 |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-DESC-01 | 嵌套 tag（tag-of-tag）多层剥皮（`#498`） | 已有 `DEFER-TT-08` 跟踪，不在本批 8 个 Issue 范围 | `#498` 重启时 | `COMPATIBILITY.md` (`#498 DEFER-TT-08`) |
| DEFER-DESC-02 | `libra describe` 配合全局 `--git-dir`/`--work-tree`（`#801`） | 需要全局仓库发现选项改造，超出 `describe` 单命令范围 | 已有计划排期实现 | `plan-20261117-global-init-infra.md`（`GII-02` 等号形式、`GII-24` 空格形式；只按 ID 引用） |
| DEFER-DESC-03 | `libra describe --contains --all` 去除分支 `heads/` 前缀（pinned Git 输出 `br-x~1`，Libra 输出 `heads/br-x~1`） | 保持 Libra 现有 `--all` 统一前缀契约，避免破坏既有 `--all` 断言 | 需统一调整 `--all` 命名空间输出时 | `COMPATIBILITY.md` (`describe` 行) |
| DEFER-DESC-04 | `libra describe --contains --exact-match`（既有差异：Git 在 `--contains` 下忽略 `--exact-match`） | 既有设计选择（`src/command/describe.rs:390-393`），不在 `#795` 变更范围 | 需与 Git `--contains --exact-match` 完全对齐时 | `COMPATIBILITY.md` (`describe` 行) |
| DEFER-DESC-05 | describe 后缀中的大写十六进制（Git 接受 `x-1-gF73C456`）与长于当前 hash 十六进制长度的后缀（pinned Git 以 `GIT_MAX_HEXSZ` 为上限，实测 SHA-1 仓库中 `x-1-g<40 位>0` 仍解析为该对象） | 依赖 `resolve_hash_atom_typed` 与对象存储前缀检索对大小写及超长前缀的统一处理，超出本批 Issue | 出现对应 Issue 或统一十六进制前缀处理时 | `COMPATIBILITY.md` (`rev-parse` 行) |
| DEFER-DESC-06 | misnamed annotated tag 在 `--all` 下的内部名与告警（Git 输出 `tags/<tag_name>-…` 并告警 `tag 'tags/<ref>' is externally known as '<tag_name>'`；本计划保持 Libra 现有引用名输出） | 不属本批 8 个 Issue（`#799` 只涉及 `--abbrev=0`），且上游 `t6120` 无对应用例 | 出现对应 Issue 时 | `COMPATIBILITY.md` (`describe` 行) |
| DEFER-DESC-07 | `--match`/`--exclude` glob 中 `*` 跨越 `/` 的语义（Git `wildmatch(…, 0)` 允许 `*` 匹配 `/`，Libra 现用 `wax` 的 `*` 不跨 `/`，`src/command/describe.rs:586`） | 属既有 tag 过滤语义差异，`DESC-03` 只把同一过滤函数扩到分支候选、不改语义；全面对齐需替换 glob 引擎，超出本批 Issue | 出现对应 Issue 或统一 glob 引擎时 | `COMPATIBILITY.md` (`describe` 行) |
| DEFER-DESC-08 | 前缀名字的其余 Git 字面查找规则：不符合 describe 语法的 `tags/`、`heads/`、`remotes/`（含 `refs/` 形式）名字在本命名空间缺失时，以完整名字命中其它命名空间引用（如分支 `tags/foo` 经 `rev-parse tags/foo` 解析，pinned Git 返回该分支，Libra 现状以 `LBR-CLI-003` 失败）；`refs/heads/<n>` 的现有查找在规范分支缺失后先把完整名字当作本地分支名、再由跨命名空间查找查 tag（Git 先 `refs/tags/refs/heads/<n>` 后 `refs/heads/refs/heads/<n>`；两者并存时 Libra 返回分支目标，pinned Git 返回 tag 目标并告警歧义，`src/utils/util.rs:2205-2209`）；以及 `refs/%s`（`refs/refs/…`，Libra 引用存储无法表示）与前缀名的 `refs/remotes/%s/HEAD`（远程名含 `/`） | 均为既有字面解析差异，不由本计划引入、与后缀解析无关：非 describe 语法名字现状以 `LBR-CLI-003` 失败（fail-closed）；`refs/heads/<n>` 的顺序差异只在完整名字同时被 tag 与本地分支使用时出现，返回其中一个确实存在的同名引用的目标（与 Git 选择不同），不返回后缀对象，现状如此、本计划不改变；`refs/%s` 不可表示；`refs/remotes/%s/HEAD` 需远程名含 `/`。describe 语法名字的跨命名空间查找已由 `ADR-DESC-03` ① 与 `DESC-10..10e` 覆盖 | 出现对应 Issue 或统一 Git DWIM 字面解析时 | `COMPATIBILITY.md` (`rev-parse` 行) |

本计划没有允许撤回或延后的任务卡：`DESC-01..08`、`DESC-01a`、`DESC-08a`、`DESC-09`、`DESC-09a..09c`、`DESC-11`、`DESC-10`、`DESC-10a..10k` 必须全部完成；外部前置未满足的卡保持 `blocked`（不以 `DEFER-*` 关单）。

## 完成判据

- [ ] 全量收口门须在**最终代码树**上执行，并记录受测 `libra rev-parse HEAD`（`HT`）、`libra rev-parse 'HEAD^{tree}'` 及结果。全量收口门在只含本计划交付提交的隔离 checkout 中执行，运行前该 checkout 的 `libra status --porcelain` 须为空。「最终代码树」指 `docs/development/plan/` 之外的全部被跟踪文件：此后只为记录门结果而修改本计划与 `plan-status.md` 的提交不视为代码树变化；判定方式为同一 checkout 中 `libra diff --old <HT> --new HEAD --name-only` 列出的每个路径都以 `docs/development/plan/` 开头，且 `libra status --porcelain` 中除 `docs/development/plan/` 下的路径外为空（未提交的被跟踪改动同样计入），否则（任何代码、测试、其它文档或配置变化）即须重跑全量收口门，与是否发生修复无关。
- [ ] 计划文本提交门：Claude（Subagent）与 Codex 对同一字节双审均为字面 `VERDICT: PASS` 且 P0/P1 全部关闭（用户 2026-11-18 指令）；各卡执行期 ER-05 review 与网站交付的独立 Claude Code `VERDICT: PASS` 另行取得，互不替代。
- [ ] 网站交付门：`DESC-11` 完成家族条目「发布点」的 AC5（评审前删除 `S_U`）与 D 组全绿后的第 2–6 步及删除核验（AC6）；其余每张卡完成第 1–3 步，并按第 3 步判定的分支完成后续：变更分支完成第 4–7 步（前端验证、独立 Claude Code `PASS`、精确暂存、签名+DCO 提交、提交树与受审字节核对、推送 `cf` 与含 `M` 的部署核验），无变更分支完成第 4 步与含 `W` 的「页面检查」；任一环节失败即 STOP 且不得标 `complete`。
- [ ] 所有任务卡满足粒度规则 `G-*`，「任务卡粒度审计表」已填齐。
- [ ] `DESC-01..08`、`DESC-01a`、`DESC-08a`、`DESC-09`、`DESC-09a..09c`、`DESC-11`、`DESC-10`、`DESC-10a..10k` 全部 `Lifecycle=done` 且 `Acceptance=complete`（ER-04）。
- [ ] 所有任务的 `Verification` 已按「验证执行协议」运行并记录；`DEP-DESC-TC` 对应脚本的记录项齐全且输出精确 `TC-<id> PASS`，之后方才进入该卡 C/D 并标 `done / complete`、关闭 Issue。`DESC-05` 在 `#541` 未合入或 `TC-1045` 未 `PASS` 前保持 `blocked`；`DESC-04` 在 `DESC-05` 未完成或 `TC-1043` 未 `PASS` 前保持 `blocked`。
- [ ] **计划完成门 / 全量收口门（ER-13）**：全部任务卡完成后运行一次 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿。
- [ ] 全量收口门暴露的 Bug 已全部前滚修复并重跑全量至全绿。
- [ ] 必要的 docs/compat/error-code/test-index 更新已完成（见「兼容与文档收口」）。
- [ ] 每张卡的 `Rollback mode` 均已核验。
- [ ] 每张会发布卡已完成 `patch + 1`、版本面同步（`compat_version_surface_sync`）、工具链刷新 `Cargo.lock`、`gh release create` 与 D 组证据取证。
- [ ] 「修订历史」已记录全部规范性变更。
- [ ] 未新建证据文件或证据文件夹（v2.12）。
- [ ] `plan-long.md` 与 `plan-status.md` 已同步。
