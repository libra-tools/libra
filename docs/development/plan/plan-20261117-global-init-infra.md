# 全局 CLI、Init 与基础设施修复计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：计划已修订（R9），尚未执行；记录时间 2026-11-17 05:14:00 UTC，全部任务 pending。
> 跟踪范围：GitHub open issues #611、#656、#686、#720、#801、#834、#875、#942–#946、#948、#987、#1014、#1019、#1039（共 17 项）。

## 文档职责

本文解决 Libra 全局 CLI 选项与别名（`-C`、`--git-dir`/`--work-tree`、`GIT_DIR`、`--bare`、`alias.*`）、`init` 仓库初始化与格式校验、`status`、`rev-parse`/`checkout` `<rev>...`、合并冲突渲染、`operation snapshot`、cloud object-index 修复队列、`add`/`fsck` 性能与 Hermes extraction 基线等 17 项基础设施与兼容缺口。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收（GC-01）。

### 适用范围

- 全局入口与仓库发现：`src/cli.rs`、`src/main.rs`、`src/utils/util.rs`、`src/internal/config.rs`。
- 命令模块：`src/command/init.rs`、`src/command/status.rs`、`src/command/status/scan.rs`、`src/command/status/output.rs`、`src/command/rev_parse.rs`、`src/command/checkout.rs`、`src/command/merge/content.rs`、`src/command/add.rs`、`src/command/fsck.rs`。
- 核心基础设施：`src/internal/operation/snapshot.rs`、`src/utils/client_storage.rs`、`src/internal/ai/capture/extraction.rs`。
- 测试与文档：`tests/command/{init,status,add,fsck,merge,rev_parse,checkout,op}_test.rs`（`command_test`）、`tests/compat/agent_architecture_guard.rs`、`docs/commands/*.md`、`docs/commands/zh-CN/*.md`、`docs/development/commands/*.md`、`COMPATIBILITY.md` 及 `/Volumes/Sea/libra-backend` 对应同步文档。

### 非目标

- 不更改 SQLite 数据库 schema 或新增 migration（GC-13 无新表变更；`init` 仅校验 `repositoryformatversion`/`extensions.*`/`core.objectformat` 配置键值）。
- 不更改 `compat_agent_architecture_guard` 冻结的 `ADR-ACF-10` 阈值（#656 必须将 `extraction.rs` 恢复至 `≤ 15` 基线，不得放宽守卫）。

### 成功定义

- 17 个关联 issue 全部在「追溯表」与任务卡中闭环，对应行为、性能与架构守卫测试通过。
- 受影响的 EN/zh/dev 命令文档、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend` 同步更新。
- 全部任务卡完成后通过 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 及 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

## 使用规则

- 每次状态推进与「修订历史」写入都记录实际 `YYYY-MM-DD HH:MM:SS UTC` 时间。
- 推进或完成任务卡时，执行者须在同一变更中同步更新 [`plan-status.md`](plan-status.md)（本计划入库时的初始登记行由协调者随本批计划合入一并登记）。
- 本文全部任务卡均未开工；任务 ID（`GII-01`..`GII-17`）固定，拆分时按 G-09 在所属 Phase 末尾追加新编号并登记「修订历史」。
- 本计划不单独新建证据目录或证据文件（v2.12），所有门禁结果、SHA 与测试摘要写在本计划内。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。以下锚点核对于 `2026-11-17 03:45:00 UTC`（`Cargo.toml` = `0.30.207`）。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 全局 CLI | `src/cli.rs`（5395 行）与 `src/main.rs` 处理全局解析、object-index 修复预检（2568-2633,3615-3640,4124-4201） | `src/cli.rs:90,2568-2633,3365,3615-3640,4124-4201` |
| `init` 入口 | `--template`（344）、`--shared`（366）、`--initial-branch` 告警（1426）、`core.objectformat`/`repositoryformatversion`（133,1327-1413,1816-1833,2375-2399） | `src/command/init.rs:105,133,197-200,342-366,722-726,807-882,1327-1426,1816-1833,2375-2399` |
| `status` 与修订 | `status` 扫描 `symlink_metadata`（`status.rs:1342-1724`, `status/scan.rs:807-873`, `status/output.rs:1303-1308`）；`get_commit_base_typed`（`util.rs:3128`） | `src/command/status.rs:1342-1724`、`src/command/status/scan.rs:807-873`、`src/utils/util.rs:3128` |
| 合并与快照 | `merge/content.rs` 冲突标记宽度与渲染（358-423）；`operation/snapshot.rs` 树路径冲突检查（1153） | `src/command/merge/content.rs:358-423`、`src/internal/operation/snapshot.rs:1153` |
| Agent 守卫 | `compat_agent_architecture_guard` 冻结 `("src/internal/ai/capture/extraction.rs", 15)`（`ADR-ACF-10`） | `tests/compat/agent_architecture_guard.rs:5839,9273` |
| 机器输出 | 各命令 `--json` 保持向后兼容，无新增 JSON schema 破坏 | `src/command/{init,status,add,fsck}.rs` |
| 错误码 | 复用 `LBR-IO-001`、`LBR-CLI-002`、`LBR-CLI-003`、`LBR-REPO-001` | `src/utils/error.rs`、`docs/error-codes.md` |
| 文档 | 中英文命令页、开发文档及 `COMPATIBILITY.md` 已存在（网站交付严格遵循 `AGENTS.md:135` 授权生产检出 `/Volumes/Sea/libra-backend` `cf` 分支：子命令页通过 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only <cmd> --libra-root /Volumes/Data/gitmono/libra` 同步，全局 CLI 选项同步更新 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx` `## Global Flags`；路径不可用时 fail-closed 停止网站交付） | `docs/commands/{init,status,add,fsck,merge,rev-parse}.md`、`COMPATIBILITY.md` |
| 测试 | `tests/command/{init,status,add,fsck,merge,rev_parse,checkout,op}_test.rs` 挂载于 `command_test`；compat：`compat_agent_architecture_guard`、`compat_rev_parse_peel_selectors` | `tests/command/mod.rs:988-1145`、`tests/INDEX.md:68,74` |
| 外部参照 | 上游 Git 测试树 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（v2.56.0-116，t3200#95、t4030） | `COMPATIBILITY.md`（2026-11-17 核对） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-GII-01 | `#686`：全局拒绝 `-C <path>`，导致 t3200#95 等工作流中止 | 无法在子目录/他处切换工作目录运行命令 | `src/cli.rs`、`src/main.rs` | GII-01 |
| GAP-GII-02 | `#801`：全局拒绝 `--git-dir` / `--work-tree` | 无法通过全局参数指定仓库与工作树 | `src/cli.rs`、`src/main.rs` | GII-02 |
| GAP-GII-03 | `#987`：忽略 `GIT_DIR`（及 `GIT_WORK_TREE`）环境变量并静默操作 cwd 仓库 | 外部脚本/Git 兼容层指向错误仓库 | `src/cli.rs`、`src/main.rs`、`src/utils/util.rs` | GII-03 |
| GAP-GII-04 | `#944`：全局拒绝 `--bare`（如 `libra --bare init` 致命失败） | 与 Git 全局 `--bare` 语法不兼容 | `src/cli.rs`、`src/command/init.rs` | GII-04 |
| GAP-GII-05 | `#945`：不展开 `alias.*` 配置（`libra <alias>` 被拒绝） | 用户自定义 Git 别名不可用 | `src/cli.rs`、`src/main.rs`、`src/internal/config.rs` | GII-05 |
| GAP-GII-06 | `#942`：`init --template` 在模板目录不存在时致命报错而非告警后继续建库 | 与 Git 容错语义不一致 | `src/command/init.rs:105,197-200,722-726` | GII-06 |
| GAP-GII-07 | `#943`：`init --shared` 拒绝 `0666` 等非可遍历数字模式而非自动补目录执行位 | 与 Git `--shared=0666` 行为不符 | `src/command/init.rs:366,807-882` | GII-07 |
| GAP-GII-08 | `#946`：`init` 重初始化告警输出 `"ignoring --initial-branch"` 而非 `"ignored --initial-branch"` | 与 Git 告警文案及上游用例断言不符 | `src/command/init.rs:1426` | GII-08 |
| GAP-GII-09 | `#948`：`init` 与仓库打开缺少 `repositoryformatversion` 与 `extensions.*` 严格校验（静默 `core.objectformat`） | 未知扩展或格式版本未 fail-closed | `src/command/init.rs:133,1327-1413,1816-1833,2375-2399` | GII-09 |
| GAP-GII-10 | `#834`：`status` 拒绝 `-v`/`--verbose`（t4030 中途失败） | 无法在 `status` 展示已暂存差异 | `src/command/status.rs` | GII-10 |
| GAP-GII-11 | `#1014`：工作区扫描跟踪符号链接在互指环（`ELOOP`）时致命失败而非告警 | 存在循环符号链接的工作区无法运行 `status` | `src/command/status.rs:1342-1724`、`src/command/status/scan.rs:807-873` | GII-11 |
| GAP-GII-12 | `#720`：不解析 `<rev>...`（三点 merge-base 语法，如 `checkout -b` 起点与 `rev-parse`） | 对称差/merge-base 语法失败 | `src/utils/util.rs:3128`、`src/command/{rev_parse,checkout}.rs` | GII-12 |
| GAP-GII-13 | `#1019`：嵌套多路冲突渲染将外层标记加宽至 9 且丢弃 `ours` 区域一行 | 嵌套冲突内容丢失且标记宽度异常 | `src/command/merge/content.rs:358-423` | GII-13 |
| GAP-GII-14 | `#1039`：`operation snapshot` 报 `tree path conflicts with file 'link'` 中止 `reset`/`init` | 含符号链接的树快照恢复失败 | `src/internal/operation/snapshot.rs:1153` | GII-14 |
| GAP-GII-15 | `#875`：间歇性 `LBR-IO-001` cloud object-index repair markers 排空失败并阻塞下一条仓库命令 | 仓库命令偶发被阻塞 | `src/cli.rs:2568-2633,4124-4201`、`src/utils/client_storage.rs` | GII-15 |
| GAP-GII-16 | `#611`：`add` 与 `fsck` 未就地哈希且未批量更新 object-index | 大仓库 `add` / `fsck` 性能落后约 25% / 7% | `src/command/add.rs`、`src/command/fsck.rs`、`src/utils/client_storage.rs` | GII-16 |
| GAP-GII-17 | `#656`：Hermes extraction 超出冻结的中立性基线（`AgentKind::` ≤ 15） | `compat_agent_architecture_guard` 架构守卫失败 | `src/internal/ai/capture/extraction.rs`、`tests/compat/agent_architecture_guard.rs:5839` | GII-17 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `issues/529.md` | 当前持有 `src/cli.rs` 写锁 | 通过 `DEP-GII-01` 等待 `issues/529#FP-13` 释放 `src/cli.rs` 写锁后再启动触及 `src/cli.rs` 的 `GII-01`..`GII-05`、`GII-15` |
| `plan-long.md` | 长期基础设施与兼容路线 | 本计划为具体执行落地，不直接改写 `plan-long.md` |
| `COMPATIBILITY.md` | 对应命令兼容矩阵 | 原地更新对应命令单元格，不破坏锚定行号 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 17 项 issue 覆盖全局参数、初始化、状态、冲突、快照、性能与 Agent 基线 | 每项独立成单轴卡 `GII-01`..`GII-17` |
| 可行性 | 全局参数/修复队列集中于 `cli.rs`/`main.rs`，`init` 集中于 `init.rs` | 同文件写集严格串行，无写集冲突卡可按 DAG 推进 |
| 任务卡粒度 | 17 张卡均为单轴、`AC ≤ 8`、`VER ≤ 8`、`scope ∈ {S, M}` | 无 L/XL 卡，无多轴卡 |
| 依赖与顺序 | `DEP-GII-01` 门控 `src/cli.rs`；共享写集（`cli.rs`、`init.rs`、`status.rs`、`util.rs`、`client_storage.rs`）均已串行 | 无环、无 I–I 并发冲突 |
| 完整性 | 每卡含 `nextest` 精确 filter、ER-13 `T-1` 全量触发、D 组真实 CI job、文档与兼容同步 | 符合 ER-04、ER-06/ER-06a、ER-08、ER-13、ER-14 |
| 安全性 | `alias.*` 防循环展开、`-C`/`--git-dir`/`GIT_DIR` 路径边界、`extensions.*` fail-closed | 继承 GC-07、GC-11 |
| 功能正确性 | 对照 `git@6de20f6092`（t3200#95、t4030）与 `ADR-ACF-10` 冻结基线 | 判据精确可测 |
| 接口兼容 | `GII-09` 严格校验 `repositoryformatversion` 与 `extensions.*` | 记录兼容窗口 |
| 数据流与控制流 | `GII-15` 修复队列原子排空、`GII-16` 批量索引事务、`GII-14` 快照冲突消解 | 继承 GC-08 |
| 性能与容量 | `GII-16` 明确基准测量目标（`add` ~25%、`fsck` ~7%） | 继承 GC-10 |
| 可靠性与容错 | `Result` + `CliError` 返回可操作诊断，无生产 `unwrap()` | 继承 GC-11 |
| 可维护性 | 共享仓库发现与修订解析收敛至 `util.rs` 单一事实源 | 继承 GC-02 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 03:45:00 UTC | 初始成稿 | 建立 `plan-20261117-global-init-infra.md`，映射全部 17 项 issue 至 `GII-01`..`GII-17` | N/A | 全文 |
| 2026-11-17 04:50:00 UTC | Codex R5 FAIL（Claude R5 `ECONNRESET`） | ① 对齐 `AGENTS.md:135`，将全计划网站交付路径、写集与命令统一为 `/Volumes/Sea/libra-backend`（`cf` 分支），并补齐前端校验、Claude Code `PASS`、提交推送与 Cloudflare 部署核验门（路径不可用时 fail-closed）；② 撤销 `GII-01`..`GII-04` 网站同步 `N/A`，将 `GII-01`..`GII-05` 纳入 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`）同卡更新；③ 更新文头状态为 `R5` | N/A | 文头、事实基线、GC-05、GII-01..05、GII-06..13、兼容收口、Review log |
| 2026-11-17 04:53:00 UTC | Codex R6 FAIL | ① 将 `ER-04/ER-08` squash-merge 树等价核验从不可用的 `git diff` 改为本仓库 `.libra` 原生 `libra fetch && libra diff --exit-code <reviewed-head> <squash-sha>`（或独立只读 Git 远端树比对）并保留签名/DCO/精确 SHA CI 门；② 完善 `ADR-GII-01` 启动预扫描契约，明确复用/共享 `src/cli.rs` 既有全局 flag 规格表（含无值标志 `--json`/`--quiet`/`--machine`/`--no-pager`/`--no-color`/`--sync-data`/`--offline`/`--exit-code-on-warning`/`--literal-pathspecs`/`--no-literal-pathspecs` 与带值标志 `--json[=<fmt>]`/`--color[=<when>]`/`--progress[=<mode>]`/`--max-connections <N>`），使既有与新增全局参数任意交错均不误判子命令；并在 `GII-01` 补入 `--json -C` / `--max-connections 8 -C` 回归验收；③ 同步追溯表 `GII-01..03,05` 网站文档与 `GII-17` `observed_agents/hermes_export.rs` | N/A | 文头、ADR-GII-01、ER-04/08、GII-01、追溯表、Review log |
| 2026-11-17 04:57:00 UTC | Codex R7 FAIL | 修正 `ADR-GII-01` 中 `--progress <MODE>` 为**必选参数值**（支持 `--progress <MODE>` 与 `--progress=<MODE>`，不可省略值），并在 `GII-01` 补入 `libra --progress text -C <dir> status` 回归验收 | N/A | 文头、ADR-GII-01、GII-01、Review log |
| 2026-11-17 05:03:00 UTC | Codex R8 FAIL（Claude R8 `ECONNRESET`） | ① 修正 `GII-12` 区分范围解析与单一起点解析：`libra rev-parse A...B` 按 Git 对称差语义输出 `<B_oid>` / `<A_oid>` / `^<merge_base_oid>`，`libra checkout -b <branch> A...B` 按单一起点语义解析为唯一 merge-base（0 或多个共同祖先报错），并分别补齐对照 Git 的测试；② 在 `GII-01` Verification 明确列入 `--progress text -C`、`--progress=text -C` 及缺值失败用例 | N/A | 文头、GII-01、GII-12、Review log |
| 2026-11-17 05:14:00 UTC | Codex R9 FAIL（Claude R9 `ECONNRESET`） | 修正 `GII-12` `rev-parse A...B` 边界语义：允许 0 个共同祖先（无关历史仅输出 `<B_oid>` 与 `<A_oid>`）及多个最佳 merge-base（criss-cross 历史输出全部 `^<mb_i>`），将「0 或多个 merge-base 报错 `LBR-CLI-003`」严格限定于 `checkout` 等单一起点上下文，并补入无关历史与 criss-cross 历史对照 Git 测试 | N/A | 文头、GII-12、Review log |
| 2026-11-17 04:24:15 UTC | Codex R4 FAIL | 将粒度审计表 `GII-17` 行为落点/生产文件计数由 `1/1` 同步为 `2/2`，与卡片内 `landing=2; prod-files=2`（`extraction.rs` + `hermes_export.rs`）一致 | N/A | 粒度表、Review log |
| 2026-11-17 04:14:20 UTC | Codex R3 FAIL | ① `GII-16` 基准改用 2,000 个互不相同文件（防内容寻址去重）并配置非交互 `user.name`/`user.email` 与 `--no-gpg-sign`；② `GII-17` 将 `observed_agents/` 目录收敛为具体文件 `src/internal/ai/observed_agents/hermes_export.rs`；③ 按 G-04 按模块/组件落点重算 `landing`（`src/cli.rs`+`src/main.rs` 计为 CLI 入口 1 落点，`GII-01/02/04` 为 2、`GII-03/05` 为 3、`GII-11` 为 2）；④ `GII-01` 移除后端概览页同步矛盾句，统一为 `N/A`；⑤ `GII-01`/`GII-05` Verification 补注 `OutputConfig` 与 `--`/别名展开覆盖用例 | N/A | 粒度表、GII-01/02/03/04/05/11/16/17、Review log |
| 2026-11-17 04:05:46 UTC | Codex R2 FAIL | ① `GII-16` 补充可独立复现的内联基准命令（明确 2,000 个 1KB 确定性文件、初始空索引/干净工作区重置、1 次预热 + 5 次计时及 `libra add .`/`libra fsck` 命令）；② `GII-01`/`GII-02`/`GII-05` 写集补入 `src/utils/output.rs`，并增加路径值形如 `--json`、`--` 后透传与别名展开后 `OutputConfig::resolve_from_argv` 的显式验收与回归用例；③ 对齐全部卡片 `landing == prod-files`（G-04） | N/A | 粒度表、GII-01/02/05..11/16、追溯表、Review log |
| 2026-11-17 03:55:00 UTC | Codex R1 FAIL | ① 全卡改为严格串行消除 `COMPATIBILITY.md`/`config.rs` 并发写集重叠；② `ADR-GII-01` 补充 `--`/参数值/引号分词/深度上限与 `OutputConfig::resolve_from_argv` 交互契约；③ `GII-16` 补充固定夹具基准阈值与事务回滚故障注入测试；④ 修复 `测试矩阵` 单元/集成命令过滤；⑤ 明确 `GII-01..03` 全局选项后端文档 `N/A` 依据及 `GII-05` 同步 `config.en.md`；⑥ 补充 squash merge 后提交签名/DCO/tree 核验及打 tag 前止步条件 | N/A | ADR-GII-01、ER-04/08、实施顺序、并发声明、粒度表、GII-01..05/09..17、测试矩阵、Review log |

## 已决议设计决策

### ADR-GII-01: 全局 `-C` / `--git-dir` / `--work-tree` / `GIT_DIR` / `--bare` / `alias.*` 预处理顺序

- **Status:** Accepted
- **Context:** #686、#801、#944、#945、#987 涉及 `src/cli.rs` 与 `src/main.rs` 的启动预处理与仓库发现顺序。
- **Decision:** 在 `main.rs`/`cli.rs` 启动管线按 Git 顺序执行：① 在遇到 `--` 或首个非全局子命令 token 前扫描全局参数：共享 `src/cli.rs` 权威全局 flag 表识别全部**既有**无值标志（`--machine`、`--no-pager`、`--no-color`、`--quiet`/`-q`、`--exit-code-on-warning`、`--sync-data`、`--offline`、`--literal-pathspecs`、`--no-literal-pathspecs`）与带值/可选值标志（`--json[=<fmt>]`/`-J`、`--color[=<when>]`）与必选值标志（`--progress <MODE>`/`--progress=<MODE>`、`--max-connections <N>`/`--max-connections=<N>`），以及**新增**全局标志（`-C <path>` / `-C<path>`、`--git-dir=<p>` / `--git-dir <p>`、`--work-tree=<p>` / `--work-tree <p>`、`--bare`，缺值或空路径返回 `LBR-CLI-002`），跳过既有全局标志的值 token 而不提前终止或误判为子命令，使 `libra --json -C <dir> status` / `libra --max-connections 8 -C <dir> status` 等任意前后交错组合均按出现顺序对 `-C` 执行 `chdir`；② 解析 `--git-dir`/`--work-tree`/`--bare` 与环境变量 `GIT_DIR`/`GIT_WORK_TREE`（CLI 优先于环境变量）；③ 若子命令非内置命令，读取 `alias.<name>` 并按 POSIX 引号/转义规则分词展开（未闭合引号报 `LBR-CLI-002`，`!` shell alias 明确报错不支持，最大展开深度 16 防环），再交由 `clap` 解析；④ `OutputConfig::resolve_from_argv`（`src/utils/output.rs:250-310`）作用于**预处理后的 argv**，并跳过 `-C`/`--git-dir`/`--work-tree` 的值及 `--` 之后参数，避免路径或透传参数被误判为输出标志。
- **Alternatives considered:** 在每个子命令内各自读取 `GIT_DIR` / `-C`（违反 GC-02，易遗漏命令，拒绝）。
- **Consequences:** `GII-01`..`GII-05` 串行修改 `src/cli.rs`/`src/main.rs` 并均受 `DEP-GII-01` 门控。
- **Revisit when:** 若后续引入内置 shell alias 或 daemon 模式再重审进程 `cwd` 切换。

### ADR-GII-02: Hermes extraction 严格回归 `ADR-ACF-10` 冻结基线（#656）

- **Status:** Accepted
- **Context:** #656 报告 `src/internal/ai/capture/extraction.rs` 的 `AgentKind::` 引用数超出 `tests/compat/agent_architecture_guard.rs:5839` 冻结上限 `15`。
- **Decision:** 将 Hermes 特定提取分支下沉至 `observed_agents/` 适配器端口，使 `extraction.rs` 保持 provider-neutral 且 `AgentKind::` 计数 ≤ 15；禁止调高 `agent_architecture_guard.rs` 中的冻结数字。
- **Alternatives considered:** 修改 `agent_architecture_guard.rs` 提高上限（违反 `ADR-ACF-10`，拒绝）。
- **Consequences:** `compat_agent_architecture_guard` 直接作为 `GII-17` 验收门。
- **Revisit when:** 仅当修订 `ADR-ACF-10` 时才可调整。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 的 `GC-01`..`GC-13`。特别强调：
- **GC-05 / ER-06a 文档同步:** 改变公开命令/全局参数行为的卡（`GII-01`..`GII-13`）同卡更新对应 `docs/commands/<cmd|README>.md`、`docs/commands/zh-CN/<cmd|README>.md`、`docs/development/commands/<cmd>.md`、`COMPATIBILITY.md`，并严格按 `AGENTS.md:135` 在授权生产网站仓库 `/Volumes/Sea/libra-backend`（`cf` 分支，保留无关改动、不建新分支/PR/worktree）同卡交付：① 子命令页运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only <cmd> --libra-root /Volumes/Data/gitmono/libra`；② 全局 CLI 选项（`GII-01`..`GII-05`）同步更新 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`）；③ 运行网站文档检查与前端验证（`gen:docs` / `typecheck` / `build`）、取得 Claude Code `PASS`、用 `libra commit` 提交并推送 `cf`、核验 Cloudflare 部署完成；若 `/Volumes/Sea/libra-backend` 不可用，fail-closed 停止网站交付。纯内部修复卡（`GII-14`、`GII-15`、`GII-16`、`GII-17`）明确记录 `N/A` 理由。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 的 `ER-01`..`ER-14`：
- **ER-04 / ER-08 / ER-13 / ER-14:** 每张 `implementation` 卡均为 `independent` 发布切片，完成后按 ER-08 执行 `patch + 1`、同步 `compat_version_surface_sync` 权威三处（`Cargo.toml`、`install.sh`、`install.ps1`）、工具链刷新 `Cargo.lock`；因每卡均修改 `install.sh`/`install.ps1`（`T-1`），C 组第 ④ 步除运行本卡 A 组 focused `nextest` 外，必须跑双环境全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`、`fmt`、`clippy`、`cargo build --release`，随后签名提交推送 PR；因本工作区为 `.libra` 仓库（无 `.git`），squash merge 生成新提交后，发布前必须执行 `libra fetch` 同步远端 `main`、核验该 squash-merge 提交 SHA 的 PGP 签名与 `Signed-off-by` DCO trailer、运行 `libra diff --exit-code <reviewed-head> <squash-sha>`（或在独立只读 Git 远端检出验证 `<reviewed-head>^{tree}` 与 `<squash-sha>^{tree}` 无差异）且 `base.yml`/`codeql.yml` 在该精确 SHA 全绿；**止步条件：** 任一核验失败立即停止，严禁执行 `gh release create "v${VERSION}"`。
- **ER-04 D 组远端后置门（每张 `independent` 卡默认适用）:**
  - `base.yml`（PR / `push` `main`）：`format`、`clippy`、`redundancy`、`test` 全部 green。
  - `codeql.yml`（PR / `push` `main`）：CodeQL 扫描 green。
  - `release.yml`（`push` `v*` tag，由 `gh release create "v${VERSION}"` 触发）：`build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` 全部 green；并核验 `download.libra.tools` / stable manifest（`HEAD` 200、range `GET` 206）。

## 实施顺序

- 全计划严格串行链（消除 `COMPATIBILITY.md`、`src/internal/config.rs`、`src/cli.rs`、`src/command/init.rs` 任何 I–I 并发重叠，G-10）：
  - Phase 0（全局 CLI）: `DEP-GII-01 -> GII-01 -> GII-02 -> GII-03 -> GII-04 -> GII-05`
  - Phase 1（`init`）: `GII-05 -> GII-06 -> GII-07 -> GII-08 -> GII-09`
  - Phase 2（核心与基础设施）: `GII-09 -> GII-10 -> GII-11 -> GII-12 -> GII-13 -> GII-14 -> GII-15 -> GII-16 -> GII-17`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-GII-01 | incoming | 跨计划写锁 | `issues/529#FP-13` | `#529` 执行者 | `issues/529.md` 释放 `src/cli.rs` 写锁 | `docs/development/plan/issues/529.md`（2026-11-17 核对） | 未释放前 `GII-01` 保持 `pending` |

### 发布分组与并发窗口

- **并发声明:** 全串行（`GII-01`..`GII-17` 单链推进，实现与发布均不并发，彻底消除共享 `COMPATIBILITY.md` / `config.rs` / `cli.rs` 写集重叠，G-10 / ER-12）。
- **发布者:** 单一指定发布者（协调者指派的执行 Agent）。
- **发布窗口顺序:** 按各卡完成顺序串行执行 `patch + 1` 与 `gh release create`，同一时刻仅允许一张卡处于已 bump 未发布状态。

### Phase 0: 全局 CLI 选项与别名（GII-01..GII-05）

**目标:** 落地全局 `-C`、`--git-dir`/`--work-tree`、`GIT_DIR`/`GIT_WORK_TREE`、`--bare` 与 `alias.*` 展开。

**进入条件:** `DEP-GII-01` 满足。

**退出条件:** `GII-01`..`GII-05` 全部 `done` / `complete`。

### Phase 1: `init` 兼容与格式校验（GII-06..GII-09）

**目标:** 修复 `init --template` 缺失目录告警、`--shared` 非可遍历权限补位、重初始化 `"ignored --initial-branch"` 文案及 `repositoryformatversion`/`extensions.*` 校验。

**进入条件:** `GII-04` 完成（释放 `init.rs`）。

**退出条件:** `GII-06`..`GII-09` 全部 `done` / `complete`。

### Phase 2: `status`、修订、冲突、快照与基础设施（GII-10..GII-17）

**目标:** 落地 `status -v`、工作区循环符号链接 `ELOOP` 告警、`<rev>...` 解析、嵌套多路冲突修复、`operation snapshot` 冲突修复、cloud object-index 队列修复、`add`/`fsck` 性能优化及 Hermes extraction 基线恢复，完成全量收口门。

**进入条件:** 各自前置依赖满足。

**退出条件:** 全部 17 张卡 `done` / `complete`，全量收口门全绿。

## 任务卡

### 任务卡粒度规则（强制）

继承 [plan-template.md](plan-template.md) 的 `G-01`..`G-11`。

### 字段全局默认与例外

- **Release boundary 默认:** `independent`（每卡独立执行完整 C 组含 `patch + 1`、`compat_version_surface_sync` 同步与 `gh release create`，并适用 ER-04 D 组：`base.yml`、`codeql.yml`、`release.yml`）
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（推送/发布前：本地 `revert` 单提交；推送/发布后：已发布版本与 tag 不可撤回，按 ER-04/ER-10 新建 `FIX-*` 前滚修复并发新 `patch + 1` 版本；兼容窗口与降级指引：同 `0.30.x` 次版本内无 schema 变更，可回退安装上一版本 `v<prev>`）
- **Migration and rollback 默认:** `N/A：无 schema 迁移`
- **Security and privacy 默认:** 继承 GC-07、GC-11（路径边界校验、防别名死循环、无裸 `unwrap()`）
- **Performance budget 默认:** 继承 GC-10
- **Release write set 默认:** `Inherited`
- **Full-suite trigger 默认:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）`
- **Deliverables 默认:** `N/A`

**默认覆盖:** `GII-14`、`GII-15`、`GII-16`、`GII-17` 的 `Docs and compatibility impact` 为 `N/A`（纯内部修复/性能/守卫，无公开 CLI 行为变更）。

**规则 waiver（`EX-*`）:** 无。

### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| GII-01 | implementation | 全局 `-C <path>` 目录切换 | immutable-release | yes | yes | 3/8 | 2/8 | 2/3 | M | DEP-GII-01 | 序列化于 DEP-GII-01 | independent | N/A | N/A |
| GII-02 | implementation | 全局 `--git-dir`/`--work-tree` | immutable-release | yes | yes | 2/8 | 2/8 | 2/3 | M | GII-01 | 序列化于 GII-01 | independent | N/A | N/A |
| GII-03 | implementation | `GIT_DIR`/`GIT_WORK_TREE` 环境变量 | immutable-release | yes | yes | 2/8 | 2/8 | 2/3 | M | GII-02 | 序列化于 GII-02 | independent | N/A | N/A |
| GII-04 | implementation | 全局 `--bare` | immutable-release | yes | yes | 2/8 | 2/8 | 2/2 | M | GII-03 | 序列化于 GII-03 | independent | N/A | N/A |
| GII-05 | implementation | `alias.*` Git 别名展开 | immutable-release | yes | yes | 3/8 | 2/8 | 3/4 | M | GII-04 | 序列化于 GII-04 | independent | N/A | N/A |
| GII-06 | implementation | `init --template` 缺失目录告警 | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | GII-05 | 序列化于 GII-05 | independent | N/A | N/A |
| GII-07 | implementation | `init --shared` 非可遍历权限补位 | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | GII-06 | 序列化于 GII-06 | independent | N/A | N/A |
| GII-08 | implementation | `init` `"ignored --initial-branch"` 文案 | immutable-release | yes | yes | 1/8 | 2/8 | 1/1 | S | GII-07 | 序列化于 GII-07 | independent | N/A | N/A |
| GII-09 | implementation | `repositoryformatversion` 与 `extensions.*` 校验 | immutable-release | yes | yes | 3/8 | 2/8 | 2/2 | M | GII-08 | 序列化于 GII-08 | independent | N/A | N/A |
| GII-10 | implementation | `status -v`/`--verbose` | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | GII-09 | 序列化于 GII-09 | independent | N/A | N/A |
| GII-11 | implementation | 工作区扫描循环符号链接 `ELOOP` 告警 | immutable-release | yes | yes | 2/8 | 2/8 | 2/3 | M | GII-10 | 序列化于 GII-10 | independent | N/A | N/A |
| GII-12 | implementation | `<rev>...` merge-base 修订解析 | immutable-release | yes | yes | 3/8 | 2/8 | 3/3 | M | GII-11 | 序列化于 GII-11 | independent | N/A | N/A |
| GII-13 | implementation | 嵌套多路冲突标记宽度与 ours 行保留 | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | S | GII-12 | 序列化于 GII-12 | independent | N/A | N/A |
| GII-14 | implementation | `operation snapshot` 符号链接/树冲突 | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | S | GII-13 | 序列化于 GII-13 | independent | N/A | N/A |
| GII-15 | implementation | cloud object-index repair markers 排空 | immutable-release | yes | yes | 3/8 | 2/8 | 2/2 | S | GII-14 | 序列化于 GII-14 | independent | N/A | N/A |
| GII-16 | implementation | `add` 与 `fsck` 就地哈希与批量索引 | immutable-release | yes | yes | 3/8 | 2/8 | 3/3 | M | GII-15 | 序列化于 GII-15 | independent | N/A | N/A |
| GII-17 | implementation | Hermes extraction 基线恢复（≤ 15） | immutable-release | yes | yes | 2/8 | 2/8 | 2/2 | S | GII-16 | 序列化于 GII-16 | independent | N/A | N/A |

---

### Task GII-01: 支持全局 `-C <path>` 目录切换（#686）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在全局 CLI 入口支持可重复的 `-C <path>` 参数，在仓库发现与子命令执行前按顺序切换工作目录（修复 t3200#95 等工作流中止问题）。

**Out of scope:** `--git-dir` / `--work-tree`（由 `GII-02` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 全局 `Cli` 结构体未注册 `-C` 参数 | `src/cli.rs`、`src/main.rs` |

**Acceptance criteria:**

- [ ] `libra -C <dir> <subcmd>`、`libra -C<dir> <subcmd>` 及多次 `-C <d1> -C <d2>` 按顺序切换目录并正确执行子命令（含 `init`、`status`、`commit` 等）；遇到 `--` 或子命令之后的参数不再被预处理为全局 `-C`。
- [ ] `OutputConfig::resolve_from_argv`（`src/utils/output.rs:250-310`）在预处理后的 argv 上运行并跳过 `-C` 路径值（如 `-C --json status` 不触发 JSON 模式）与 `--` 后参数（如 `status -- --json` 不触发 JSON 模式）；且既有全局标志（无值如 `--quiet`/`--no-pager` 与带值如 `--json=ndjson`/`--max-connections 8`）置于 `-C <dir>` 之前或之后时（如 `libra --json -C <dir> status`、`libra --max-connections 8 -C <dir> status`、`libra --progress text -C <dir> status`、`libra --progress=text -C <dir> status`）均正确切换目录并输出对应格式。
- [ ] 目标目录不存在或不可访问时返回 `LBR-IO-001` 明确错误。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`、`COMPATIBILITY.md` 及 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`），并按 `GC-05` 完成 `/Volumes/Sea/libra-backend` `cf` 前端校验、Claude Code `PASS`、提交推送与部署核验。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::global_dash_c_directory)'`（`(new)`，含 `-C --json status` 路径名歧义、`status -- --json` 透传及 `--progress text -C`/`--progress=text -C`/缺值失败回归用例）
- [ ] `source .env.test && cargo nextest run --lib cli`

**Full-suite trigger:** `T-1: 触及 src/cli.rs 全局 flag 注册及版本面集合` | **Dependencies:** `DEP-GII-01`

**Implementation write set:** `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `tests/command/init_test.rs`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `tests/command/init_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags` 记录 `-C`），并按 `GC-05` 完成 `/Volumes/Sea/libra-backend` `cf` 前端校验、Claude Code `PASS`、提交推送与 Cloudflare 部署核验。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=全局-C目录切换; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=3; scope=M; deps=DEP-GII-01; writeset=序列化于DEP-GII-01; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-02: 支持全局 `--git-dir` 与 `--work-tree`（#801）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在全局 CLI 入口支持 `--git-dir=<path>` 与 `--work-tree=<path>`，覆盖默认 `.libra` 仓库目录与工作树根目录解析。

**Out of scope:** `GIT_DIR`/`GIT_WORK_TREE` 环境变量（由 `GII-03` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 全局 `Cli` 未注册 `--git-dir` / `--work-tree` | `src/cli.rs`、`src/main.rs` |

**Acceptance criteria:**

- [ ] `libra --git-dir=<dir> --work-tree=<wt> <cmd>` 与 `--git-dir <dir> --work-tree <wt> <cmd>` 正确在指定仓库与工作树上执行，且 `OutputConfig::resolve_from_argv` 跳过 `--git-dir`/`--work-tree` 路径值（如 `--git-dir --json`）及 `--` 后参数。
- [ ] 无效 `--git-dir` 或 `--work-tree` 返回 `LBR-REPO-001` / `LBR-IO-001` 错误。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`），并按 `GC-05` 完成 `/Volumes/Sea/libra-backend` `cf` 前端校验、Claude Code `PASS`、提交推送与部署核验。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::status_test::global_git_dir_and_work_tree)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --lib cli`

**Full-suite trigger:** `T-1: 触及 src/cli.rs 全局 flag 注册及版本面集合` | **Dependencies:** `GII-01`

**Implementation write set:** `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `tests/command/status_test.rs`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `tests/command/status_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags` 记录 `--git-dir`/`--work-tree`），并按 `GC-05` 完成 `/Volumes/Sea/libra-backend` `cf` 前端校验、Claude Code `PASS`、提交推送与部署核验。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=全局--git-dir/--work-tree; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=3; scope=M; deps=GII-01; writeset=序列化于GII-01; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-03: 支持 `GIT_DIR` / `GIT_WORK_TREE` 环境变量（#987）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在仓库发现管线（`util.rs` 与 `cli.rs`/`main.rs`）识别 `GIT_DIR` 与 `GIT_WORK_TREE` 环境变量（优先级低于显式 `--git-dir`/`--work-tree`），不再静默回落操作 cwd 仓库。

**Out of scope:** 全局 `--bare`（由 `GII-04` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 仓库发现忽略 `GIT_DIR` 并直接从 cwd 向上查找 `.libra` | `src/cli.rs`、`src/utils/util.rs` |

**Acceptance criteria:**

- [ ] 设置 `GIT_DIR` 时所有仓库命令均作用于 `GIT_DIR` 指向的仓库而非 cwd 仓库。
- [ ] 显式 `--git-dir` 优先级高于 `GIT_DIR`；`GIT_DIR` 指向不存在仓库时 fail-closed 返回 `LBR-REPO-001`（不静默回落 cwd）。
- [ ] ER-06/ER-06a 同卡强制门：同步受影响文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::status_test::git_dir_env_overrides_cwd)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --lib util`

**Full-suite trigger:** `T-1: 触及 src/cli.rs / src/utils/util.rs 共享仓库发现及版本面集合` | **Dependencies:** `GII-02`

**Implementation write set:** `src/cli.rs`, `src/main.rs`, `src/utils/util.rs`, `tests/command/status_test.rs`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/main.rs`, `src/utils/util.rs`, `tests/command/status_test.rs`

**Docs and compatibility impact:** 更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags` 说明 `GIT_DIR`/`GIT_WORK_TREE`），并按 `GC-05` 完成 `/Volumes/Sea/libra-backend` `cf` 前端校验、Claude Code `PASS`、提交推送与部署核验。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=GIT_DIR/GIT_WORK_TREE环境变量; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=3; scope=M; deps=GII-02; writeset=序列化于GII-02; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-04: 支持全局 `--bare` 选项（#944）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在全局 `Cli` 入口支持 `--bare` 选项，并将其传递至仓库发现上下文与 `init` 等子命令，使 `libra --bare init` 等价于 `libra init --bare`。

**Out of scope:** `init --template` / `--shared`（分别由 `GII-06`、`GII-07` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--bare` 仅注册在 `init` 子命令上，全局 `libra --bare init` 被拒绝 | `src/cli.rs`、`src/command/init.rs` |

**Acceptance criteria:**

- [ ] `libra --bare init <dir>` 成功创建裸仓库。
- [ ] `libra --bare <cmd>` 将当前/指定仓库视为裸仓库上下文。
- [ ] ER-06/ER-06a 同卡强制门：同步 `init` 与全局文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::global_bare_init)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::init_test::bare)'`

**Full-suite trigger:** `T-1: 触及 src/cli.rs 全局 flag 注册及版本面集合` | **Dependencies:** `GII-03`

**Implementation write set:** `src/cli.rs`, `src/command/init.rs`, `tests/command/init_test.rs`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/command/init.rs`, `tests/command/init_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 更新 `docs/commands/{README,init}.md`、`docs/commands/zh-CN/{README,init}.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags` 记录 `--bare`），并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only init --libra-root /Volumes/Data/gitmono/libra`，按 `GC-05` 完成前端校验、Claude Code `PASS`、提交推送与部署核验。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=全局--bare选项; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=GII-03; writeset=序列化于GII-03; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-05: 支持 `alias.*` Git 别名展开（#945）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `src/cli.rs` / `src/main.rs` 识别未匹配内置子命令的首个参数，读取严格级联配置 `alias.<name>`（仅非 `!` shell 外部别名或按 Git 规则安全处理），按 shell-words 分词展开并与剩余参数拼接后重新送入 CLI 解析，带最大递归深度上限防止循环别名死循环。

**Out of scope:** 不执行未受控任意 shell `!` 别名（若遇 `!` 前缀按需明确支持或 fail-closed 报错并记录在文档）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `cli.rs` 未读取 `alias.*` 配置，未知子命令直接被 `clap` 拒绝 | `src/cli.rs`、`src/internal/config.rs` |

**Acceptance criteria:**

- [ ] `libra config alias.st "status --short"` 后运行 `libra st` 正确按 POSIX 引号分词展开并执行，且 `OutputConfig::resolve_from_argv` 作用于**别名展开后的 argv**（如 `alias.sj="status --json"` 运行 `libra sj` 输出 JSON，且别名内 `--` 后参数不误触发全局输出标志）。
- [ ] 循环别名（如 `a -> b -> a`，深度 > 16）、未闭合引号及 `!cmd` shell alias 均 fail-closed 返回 `LBR-CLI-002`。
- [ ] 不允许别名覆盖既有内置子命令（匹配 Git）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新 `docs/commands/{README,config}.md`、`docs/commands/zh-CN/{README,config}.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md` 及 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/{config.en.md,index.en.mdx}`，并按 `GC-05` 完成 `/Volumes/Sea/libra-backend` `cf` 前端校验、Claude Code `PASS`、提交推送与部署核验。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::config_test::git_alias_expansion)'`（`(new)`，含引号分词、循环深度、未闭合引号、`!cmd`、全局标志组合及 `alias.sj="status --json"` 展开后 `OutputConfig::resolve_from_argv` 解析用例）
- [ ] `source .env.test && cargo nextest run --lib cli`

**Full-suite trigger:** `T-1: 触及 src/cli.rs 命令分发及版本面集合` | **Dependencies:** `GII-04`

**Implementation write set:** `src/cli.rs`, `src/main.rs`, `src/internal/config.rs`, `src/utils/output.rs`, `tests/command/config_test.rs`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/main.rs`, `src/internal/config.rs`, `src/utils/output.rs`, `tests/command/config_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/{README,config}.md`、`docs/commands/zh-CN/{README,config}.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only config --libra-root /Volumes/Data/gitmono/libra`，按 `GC-05` 完成前端校验、Claude Code `PASS`、提交推送与部署核验。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=alias.*_Git别名展开; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=3; prod-files=4; scope=M; deps=GII-04; writeset=序列化于GII-04; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-06: `init --template` 缺失模板目录降级为告警并继续建库（#942）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 修改 `init.rs` 中 `--template` 目录不存在时的处理：不再作为致命错误中止建库，而是打印 `warning: templates not found in <path>` 至 stderr 并继续创建仓库（匹配 Git）。

**Out of scope:** 自引用/仓库内递归模板安全检查（`init.rs:737-775`）保持 fail-closed 不变。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `init.rs` 在模板目录不存在时返回致命错误 `template directory '...' does not exist` | `src/command/init.rs:105,197-200,722-726` |

**Acceptance criteria:**

- [ ] `libra init --template=/nonexistent <dir>` 打印告警到 stderr 且退出码为 0 并成功创建仓库。
- [ ] 自引用模板路径（如 `--template=.`）安全守卫仍拒绝并失败。
- [ ] ER-06/ER-06a 同卡强制门：同步 `init` 文档与 `COMPATIBILITY.md`。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::missing_template_warns_and_succeeds)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::init_test::template)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-05`

**Implementation write set:** `src/command/init.rs`, `tests/command/init_test.rs`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/init.rs`, `tests/command/init_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only init --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=init_--template缺失目录告警; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=GII-05; writeset=序列化于GII-05; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-07: `init --shared` 为非可遍历数字模式自动补目录执行位（#943）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `init.rs` 的 `resolve_shared_mode` / `apply_shared` 中，接受如 `0666`、`0660` 等合法八进制共享权限模式，并按 Git 规则对目录自动补齐对应读权限位对应的执行位（`(mode & 0o444) >> 2`）。

**Out of scope:** 不放宽非法八进制或过宽/越界字符串校验。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_shared_mode` 拒绝 `0666` 等未带执行位的数字模式 | `src/command/init.rs:366,807-882` |

**Acceptance criteria:**

- [ ] `libra init --shared=0666 <dir>` 成功创建仓库，目录权限含执行位（可遍历），文件保持 `0666` 掩码语义。
- [ ] 非八进制或越界模式仍返回 `LBR-CLI-002` 错误。
- [ ] ER-06/ER-06a 同卡强制门：同步 `init` 文档与 `COMPATIBILITY.md`。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::shared_numeric_mode_adds_dir_exec_bits)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::init_test::shared)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-06`

**Implementation write set:** `src/command/init.rs`, `tests/command/init_test.rs`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/init.rs`, `tests/command/init_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only init --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=init_--shared数字权限目录执行位; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=GII-06; writeset=序列化于GII-06; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-08: 统一 `init` 重初始化告警文案 `"ignored --initial-branch"`（#946）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 将 `src/command/init.rs:1426` 重初始化时输出的 `"ignoring --initial-branch ..."` 修正为 Git 一致的 `"ignored --initial-branch ..."`。

**Out of scope:** 不改变重初始化保持既有默认分支的行为。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `init.rs` 输出 `"ignoring --initial-branch '{requested}' on re-initialization; keeping '{initial_branch}'"` | `src/command/init.rs:1426` |

**Acceptance criteria:**

- [ ] `libra init -b foo` 重初始化已有仓库时 stderr 包含 `ignored --initial-branch`。
- [ ] ER-06/ER-06a 同卡强制门：核对并同步 `init` 文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::reinit_ignored_initial_branch_warning)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::init_test::initial_branch)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-07`

**Implementation write set:** `src/command/init.rs`, `tests/command/init_test.rs`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/init.rs`, `tests/command/init_test.rs`

**Docs and compatibility impact:** 逐文件核对并同步 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only init --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `S` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=init重初始化--initial-branch告警文案; recovery=immutable-release; complete=yes; self-contained=yes; AC=1/8; VER=2/8; landing=1; prod-files=1; scope=S; deps=GII-07; writeset=序列化于GII-07; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-09: 补全 `repositoryformatversion` 与 `extensions.*` 严格校验（#948）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `init.rs` 与仓库配置校验中补全 `core.repositoryformatversion` 与 `extensions.*`（含 `extensions.objectformat`）验证：version=0 时忽略或按规范拒绝不应存在的扩展组合，version=1 时校验已知扩展并 fail-closed 拒绝未知扩展，禁止静默接受非法 `core.objectformat`。

**Out of scope:** 不新增 SQLite schema migration（仅校验 config KV 键）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `init.rs` 写入 `repositoryformatversion=0` 与 `core.objectformat`，但缺少通用 `repositoryformatversion`/`extensions.*` 校验 | `src/command/init.rs:133,1327-1413,1816-1833,2375-2399` |

**Acceptance criteria:**

- [ ] `core.repositoryformatversion > 1` 或 `version = 1` 下含未知 `extensions.*` 时仓库操作 fail-closed 报错。
- [ ] `core.repositoryformatversion=1` + `extensions.objectformat=sha256` 正确校验并与 `core.objectformat` 一致。
- [ ] 非法或矛盾 `core.objectformat` 不被静默忽略，返回明确错误。
- [ ] ER-06/ER-06a 同卡强制门：同步 `init` 文档与 `COMPATIBILITY.md`。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::repositoryformatversion_and_extensions_validation)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::init_test::object_format)'`

**Full-suite trigger:** `T-1: 触及共享 config/仓库校验及版本面集合` | **Dependencies:** `GII-08`

**Implementation write set:** `src/command/init.rs`, `src/internal/config.rs`, `tests/command/init_test.rs`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/init.rs`, `src/internal/config.rs`, `tests/command/init_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only init --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=repositoryformatversion与extensions.*校验; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=GII-08; writeset=序列化于GII-08; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-10: 实现 `status -v` / `--verbose`（#834）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `StatusArgs` 新增 `-v`/`--verbose`（可重复：`-v` 输出已暂存变更的 unified diff；`-vv` 额外输出工作区未暂存变更 diff），不修改 HEAD 或索引状态（修复 t4030 中因拒绝 `-v` 导致 HEAD 未恢复的问题）。

**Out of scope:** 工作区循环符号链接 `ELOOP`（由 `GII-11` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `status.rs` 未注册 `-v`/`--verbose` 旗标 | `src/command/status.rs:561` |

**Acceptance criteria:**

- [ ] `libra status -v` 在标准输出附带已暂存变更（HEAD vs index）的 unified diff。
- [ ] `libra status -vv` 额外附带未暂存变更（index vs worktree）的 unified diff。
- [ ] ER-06/ER-06a 同卡强制门：同步 `status` 文档与 `COMPATIBILITY.md`。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::status_test::verbose_staged_and_unstaged_diff)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::status_test::)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-09`

**Implementation write set:** `src/command/status.rs`, `tests/command/status_test.rs`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/status.rs`, `tests/command/status_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`COMPATIBILITY.md`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only status --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=status_-v/--verbose; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=GII-09; writeset=序列化于GII-09; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-11: 工作区扫描不跟随循环符号链接，遇 `ELOOP` 告警不中止（#1014）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 排查并修复工作区扫描中跟随符号链接导致互指符号链接触发致命 `ELOOP` 的问题，确保始终使用 `symlink_metadata` 不递归穿透符号链接，并在操作系统返回 `ELOOP` 时输出告警继续完成扫描（匹配 Git）。

**Out of scope:** 不改变普通有效符号链接的 `120000` mode 记录。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 工作区扫描与状态输出路径使用 `symlink_metadata` 但部分路径仍会因解引用触发 `ELOOP` | `src/command/status.rs:1342-1724`、`src/command/status/scan.rs:807-873`、`src/command/status/output.rs:1303-1308` |

**Acceptance criteria:**

- [ ] 包含互指符号链接（`a -> b`, `b -> a`）的工作区运行 `libra status` 退出 0 并输出合理告警/状态而不致命失败。
- [ ] 正常符号链接仍按 `120000` 正确跟踪。
- [ ] ER-06/ER-06a 同卡强制门：同步 `status` 文档与 `COMPATIBILITY.md`。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::status_test::mutually_referential_symlinks_no_eloop)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::status_test::symlink)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-10`

**Implementation write set:** `src/command/status.rs`, `src/command/status/scan.rs`, `src/command/status/output.rs`, `tests/command/status_test.rs`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/status.rs`, `src/command/status/scan.rs`, `src/command/status/output.rs`, `tests/command/status_test.rs`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`COMPATIBILITY.md`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only status --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=工作区扫描循环符号链接ELOOP告警; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=3; scope=M; deps=GII-10; writeset=序列化于GII-10; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-12: 支持 `<rev>...` 三点 merge-base 修订解析（#720）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 区分 `<rev1>...<rev2>`（含省略一侧默认为 `HEAD`）的两种 Git 语义并在 `util::get_commit_base_typed` / `rev-parse` / `checkout` 中分别实现：① **范围解析（`libra rev-parse A...B`）**按 Git 对称差语义展开输出正/负修订（`<B_oid>`、`<A_oid>` 以及零个或多个最佳 merge-base `^<mb_i>`：无共同祖先时不输出 `^` 排除项，criss-cross 历史输出全部最佳 merge-base）；② **单一起点解析（`libra checkout -b <branch> <start_point>` 等单提交上下文）**解析为 `A` 与 `B` 的唯一 merge-base 提交（仅在单一起点上下文中，存在多个或零个共同祖先时按 Git 规则 fail-closed 报错 `LBR-CLI-003`）。

**Out of scope:** 不改变 `log A...B` 的对称差遍历语义。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `get_commit_base_typed` 未处理 `<rev1>...<rev2>` merge-base 语法 | `src/utils/util.rs:3128` |

**Acceptance criteria:**

- [ ] `libra rev-parse A...B`、`A...`、`...B` 按 Git 对称差规则输出 `<B_oid>`、`<A_oid>` 与零个（无关历史）或全部最佳 merge-base 排除项 `^<mb_i>`（criss-cross 历史）。
- [ ] `libra checkout -b topic A...B`（及单一起点 `get_commit_base_typed` 上下文）解析为 `A` 与 `B` 的唯一 merge-base 并以此创建切换分支；仅在该单一起点上下文中无共同祖先或存在多个 merge-base 时返回 `LBR-CLI-003` 明确错误。
- [ ] ER-06/ER-06a 同卡强制门：同步 `rev-parse`、`checkout` 文档与 `COMPATIBILITY.md`。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::triple_dot_merge_base) | test(=command::checkout_test::checkout_b_triple_dot_merge_base)'`（`(new)`，分别对照 `git rev-parse A...B`（含单 merge-base、无关历史零 merge-base 与 criss-cross 多 merge-base）及 `git checkout -b topic A...B`（含单 merge-base 成功与 0/多 merge-base 失败））
- [ ] `source .env.test && cargo nextest run --test compat_rev_parse_peel_selectors`

**Full-suite trigger:** `T-1: 触及共享 src/utils/util.rs 修订解析及版本面集合` | **Dependencies:** `GII-11`

**Implementation write set:** `src/utils/util.rs`, `src/command/rev_parse.rs`, `src/command/checkout.rs`, `tests/command/rev_parse_test.rs`, `tests/command/checkout_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/utils/util.rs`, `src/command/rev_parse.rs`, `src/command/checkout.rs`, `tests/command/rev_parse_test.rs`, `tests/command/checkout_test.rs`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/{rev-parse,checkout}.md`、`docs/commands/zh-CN/{rev-parse,checkout}.md`、`COMPATIBILITY.md`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）分别同步 `rev-parse` 与 `checkout`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=<rev>..._merge-base修订解析; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=GII-11; writeset=序列化于GII-11; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-13: 修复嵌套多路冲突标记宽度（外层 9）与 `ours` 区域丢行（#1019）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 修复 `src/command/merge/content.rs:358-423` 在嵌套多路冲突物化时错误将外层标记加宽至 9 且丢弃 `ours` 区域首行/末行的缺陷，使嵌套冲突标记长度与行内容完全匹配 Git。

**Out of scope:** 不改变单层 `merge`/`diff3`/`zdiff3` 样式配置。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `unambiguous_conflict_marker_length` 与 `extra_marker_size` 计算外层标记长度并渲染冲突块 | `src/command/merge/content.rs:358-423` |

**Acceptance criteria:**

- [ ] 嵌套多路冲突物化后外层标记宽度与 Git 一致，不无故扩宽至 9。
- [ ] `ours` 区域全部行完整保留，无行丢失。
- [ ] ER-06/ER-06a 同卡强制门：核对并同步 `merge` 文档与 `COMPATIBILITY.md`。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::nested_multiway_conflict_preserves_ours_and_markers)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::merge_test::conflict)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-12`

**Implementation write set:** `src/command/merge/content.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/content.rs`, `tests/command/merge_test.rs`

**Docs and compatibility impact:** 逐文件核对并同步 `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only merge --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `S` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=嵌套多路冲突标记宽度与ours行保留; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=S; deps=GII-12; writeset=序列化于GII-12; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-14: 修复 `operation snapshot` 符号链接路径冲突中止 `reset`/`init`（#1039）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 修复 `src/internal/operation/snapshot.rs:1153` 构建或恢复快照树时把符号链接 `link` 及其同名子树路径误判为 `"tree path conflicts with file 'link'"` 导致 `reset`/`init` 中止的缺陷。

**Out of scope:** 不改变真实文件与目录同名冲突的合法拒绝。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `snapshot.rs:1153` 报 `"tree path conflicts with file '{first}'"` | `src/internal/operation/snapshot.rs:1153` |

**Acceptance criteria:**

- [ ] 包含符号链接 `link` 的工作区在 `reset`/`init` 触发 `operation snapshot` 时不再误报路径冲突。
- [ ] 真实的文件与目录同名冲突仍正确拒绝。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::op_test::snapshot_symlink_no_false_tree_conflict)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --lib operation::snapshot`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-13`

**Implementation write set:** `src/internal/operation/snapshot.rs`, `tests/command/op_test.rs`

**Release write set:** `Inherited` | **Files likely touched:** `src/internal/operation/snapshot.rs`, `tests/command/op_test.rs`

**Docs and compatibility impact:** `N/A`：纯内部快照树构建缺陷修复，无公开 CLI 参数或用户文档变化。

**Rollback mode:** `immutable-release` | **Estimated scope:** `S` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=operation_snapshot符号链接路径冲突; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=S; deps=GII-13; writeset=序列化于GII-13; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-15: 修复间歇性 `LBR-IO-001` cloud object-index repair markers 排空与阻塞（#875）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 定位并修复 `src/cli.rs:2579-2633` 与 `src/utils/client_storage.rs` 中 cloud object-index repair markers 在无竞争或并发窗口下未能完整排空并间歇性以 `LBR-IO-001` 阻塞后续仓库命令的问题（保持 `libra cloud sync` 与破坏性清理的 fail-closed 边界）。

**Out of scope:** 不削弱 `libra cloud sync` 对未排空修复队列的 fail-closed 门禁。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `repair_pending_object_index_updates_before_command` 在预检失败或有界未排空时返回错误并附提示 | `src/cli.rs:2568-2633,4124-4201` |

**Acceptance criteria:**

- [ ] 正常本地仓库命令在 cloud repair marker 存在且可排空时完整排空并成功执行，不间歇性报 `LBR-IO-001`。
- [ ] 并发锁竞争或真正 I/O 失败仍准确记录诊断并保持 `cloud` 操作 fail-closed。
- [ ] 新增并发/重放回归测试稳定通过（无 flaky）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib cli::tests::object_index_repair`（含 `(new)` `repair_markers_drain_without_spurious_lbr_io_001`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(cloud)'`

**Full-suite trigger:** `T-1: 触及 src/cli.rs 预检管线及版本面集合` | **Dependencies:** `GII-14`

**Implementation write set:** `src/cli.rs`, `src/utils/client_storage.rs`

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/utils/client_storage.rs`

**Docs and compatibility impact:** `N/A`：修复内部修复队列排空竞态，不改变公开 CLI 参数或 `LBR-IO-001` 错误码定义。

**Rollback mode:** `immutable-release` | **Estimated scope:** `S` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=cloud_object-index修复队列排空; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=2; scope=S; deps=GII-14; writeset=序列化于GII-14; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-16: `add` 与 `fsck` 就地哈希与 object-index 批量更新性能优化（#611）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 优化 `add.rs` 与 `fsck.rs` 的对象写入与索引更新路径：对象就地计算哈希（避免多余中间缓冲/重复读写），并将 `client_storage.rs` 的 object-index 更新合并为事务批量更新，实现 `add` 约 25%、`fsck` 约 7% 性能提升并附实测基准数据。

**Out of scope:** 不改变对象格式、哈希算法或 `fsck` 校验覆盖面。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `add.rs` 与 `fsck.rs` 逐个对象哈希并逐条更新 object-index | `src/command/add.rs`、`src/command/fsck.rs`、`src/utils/client_storage.rs` |

**Acceptance criteria:**

- [ ] `add` 与 `fsck` 实现就地哈希与批量 object-index 更新，功能与错误语义完全不变。
- [ ] 批量事务失败时整体回滚，不留半更新索引（由故障注入测试验证）。
- [ ] 固定基准协议达标：以 `GII-15` 完成后的基线提交为对照，在同机 `cargo build --release` 下对确定性 2,000 个 1KB 文件仓（每次 `add` 从干净空索引重跑、`fsck` 在已提交仓运行；1 次预热 + 5 次计时取中位数），`libra add .` 耗时降低 `≥ 15%`、`libra fsck` 耗时降低 `≥ 5%`（记录基线 SHA、硬件参数与中位耗时）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::add_test::test_add_batch_object_index_rollback_on_tx_failure) | test(command::add_test::) | test(command::fsck_test::)'`（`(new)` 含事务失败注入回滚用例）
- [ ] `python3 -c "import tempfile,pathlib,subprocess,time,statistics,shutil; b=str(pathlib.Path('target/release/libra').resolve()); d=tempfile.mkdtemp(); [pathlib.Path(d,f'f_{i:04d}.txt').write_bytes(f'file-{i:04d}-'.encode()+b'x'*1014) for i in range(2000)]; add_t=[]; [((shutil.rmtree(pathlib.Path(d,'.libra'),ignore_errors=True), subprocess.run([b,'init'],cwd=d,check=True)), t0:=time.perf_counter(), subprocess.run([b,'add','.'],cwd=d,check=True), add_t.append(time.perf_counter()-t0)) for _ in range(6)]; subprocess.run([b,'config','user.name','Bench'],cwd=d,check=True); subprocess.run([b,'config','user.email','bench@example.com'],cwd=d,check=True); subprocess.run([b,'commit','--no-gpg-sign','-m','bench'],cwd=d,check=True); fsck_t=[]; [(t0:=time.perf_counter(), subprocess.run([b,'fsck'],cwd=d,check=True), fsck_t.append(time.perf_counter()-t0)) for _ in range(6)]; print('add_med=',statistics.median(add_t[1:]),'fsck_med=',statistics.median(fsck_t[1:])); shutil.rmtree(d)"`（分别在基线 `HEAD` 与本卡 `--release` 构建运行，验证 `add` 中位耗时降幅 `≥ 15%`、`fsck` 中位耗时降幅 `≥ 5%`）

**Full-suite trigger:** `T-1: 触及共享 client_storage.rs object-index 及版本面集合` | **Dependencies:** `GII-15`

**Implementation write set:** `src/command/add.rs`, `src/command/fsck.rs`, `src/utils/client_storage.rs`, `tests/command/add_test.rs`, `tests/command/fsck_test.rs`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/add.rs`, `src/command/fsck.rs`, `src/utils/client_storage.rs`, `tests/command/add_test.rs`, `tests/command/fsck_test.rs`

**Docs and compatibility impact:** `N/A`：纯内部热路径性能优化，无公开 CLI 或输出格式变化。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=add/fsck就地哈希与批量索引; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=GII-15; writeset=序列化于GII-15; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-17: 修复 Hermes extraction 超出冻结 provider-neutrality 基线（#656）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 `ADR-GII-02` 重构 `src/internal/ai/capture/extraction.rs` 中 Hermes 相关逻辑，将其下沉至 `observed_agents/` 适配器端口，使 `extraction.rs` 内 `AgentKind::` 引用数恢复至冻结基线 `≤ 15`（不得调高 `agent_architecture_guard.rs` 阈值）。

**Out of scope:** 不修改 `tests/compat/agent_architecture_guard.rs:5839` 的冻结常数 `15`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `compat_agent_architecture_guard` 冻结 `("src/internal/ai/capture/extraction.rs", 15)` | `tests/compat/agent_architecture_guard.rs:5839,9273`、`src/internal/ai/capture/extraction.rs` |

**Acceptance criteria:**

- [ ] `src/internal/ai/capture/extraction.rs` 的 `AgentKind::` 计数 ≤ 15，且 Hermes 提取功能无退化。
- [ ] `compat_agent_architecture_guard` 全部守卫通过且未放宽任何冻结基线。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test compat_agent_architecture_guard`
- [ ] `source .env.test && cargo nextest run --lib ai::capture::extraction`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-16`

**Implementation write set:** `src/internal/ai/capture/extraction.rs`, `src/internal/ai/observed_agents/hermes_export.rs`

**Release write set:** `Inherited` | **Files likely touched:** `src/internal/ai/capture/extraction.rs`, `src/internal/ai/observed_agents/hermes_export.rs`

**Docs and compatibility impact:** `N/A`：纯内部架构中立性修复，无公开 CLI 或用户文档变化。

**Rollback mode:** `immutable-release` | **Estimated scope:** `S` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=Hermes_extraction基线恢复; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=2; scope=S; deps=GII-16; writeset=序列化于GII-16; release=independent; split-from=N/A; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `cli`、`util`、`operation::snapshot`、`ai::capture::extraction` | `source .env.test && cargo nextest run --lib -E 'test(cli) | test(util) | test(operation::snapshot) | test(ai::capture::extraction)'` |
| 集成 | `init`、`status`、`add`、`fsck`、`merge`、`rev_parse`、`checkout`、`op`、`config` | `source .env.test && cargo nextest run --test command_test -E 'test(command::init_test::) | test(command::status_test::) | test(command::add_test::) | test(command::fsck_test::) | test(command::merge_test::) | test(command::rev_parse_test::) | test(command::checkout_test::) | test(command::op_test::) | test(command::config_test::)'` |
| 兼容 | `agent_architecture_guard`、`rev_parse_peel_selectors` | `source .env.test && cargo nextest run --test compat_agent_architecture_guard --test compat_rev_parse_peel_selectors` |
| 迁移 | N/A（无 SQLite schema 变更） | N/A |
| 安全 | `--template` 自引用、循环 `alias.*`、循环符号链接 `ELOOP`、`extensions.*` fail-closed | 同上单元与集成用例 |
| 性能 | `add` ~25% 与 `fsck` ~7% 提速实测 | 同机基准对照（GII-16） |
| live/gated | 每卡 T-1 与收口阶段双环境全量回归 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| GII-01 | #686; `src/cli.rs`, `src/main.rs` | `src/cli.rs`, `src/main.rs` | `README.md` EN/zh + `COMPATIBILITY.md` + 后端 `commands/index.en.mdx` | `command::init_test::global_dash_c_directory` `(new)` |
| GII-02 | #801; `src/cli.rs`, `src/main.rs` | `src/cli.rs`, `src/main.rs` | 同上 | `command::status_test::global_git_dir_and_work_tree` `(new)` |
| GII-03 | #987; `src/cli.rs`, `src/utils/util.rs` | `src/cli.rs`, `src/main.rs`, `src/utils/util.rs` | 同上 | `command::status_test::git_dir_env_overrides_cwd` `(new)` |
| GII-04 | #944; `src/cli.rs`, `src/command/init.rs` | `src/cli.rs`, `src/command/init.rs` | `init.md` EN/zh/dev + `README.md` + `COMPATIBILITY.md` + 后端 `init.en.md` / `index.en.mdx` | `command::init_test::global_bare_init` `(new)` |
| GII-05 | #945; `src/cli.rs`, `src/internal/config.rs` | `src/cli.rs`, `src/main.rs`, `src/internal/config.rs` | `README.md`/`config.md` EN/zh + `COMPATIBILITY.md` + 后端 `config.en.md` / `index.en.mdx` | `command::config_test::git_alias_expansion` `(new)` |
| GII-06 | #942; `init.rs:105,197-200,722` | `src/command/init.rs` | `init.md` EN/zh/dev + `COMPATIBILITY.md` + 后端 | `command::init_test::missing_template_warns_and_succeeds` `(new)` |
| GII-07 | #943; `init.rs:366,807-882` | `src/command/init.rs` | 同上 | `command::init_test::shared_numeric_mode_adds_dir_exec_bits` `(new)` |
| GII-08 | #946; `init.rs:1426` | `src/command/init.rs` | 同上 | `command::init_test::reinit_ignored_initial_branch_warning` `(new)` |
| GII-09 | #948; `init.rs:133,1327-1413,2375` | `src/command/init.rs`, `src/internal/config.rs` | 同上 | `command::init_test::repositoryformatversion_and_extensions_validation` `(new)` |
| GII-10 | #834; `status.rs:561` | `src/command/status.rs` | `status.md` EN/zh/dev + `COMPATIBILITY.md` + 后端 | `command::status_test::verbose_staged_and_unstaged_diff` `(new)` |
| GII-11 | #1014; `status.rs:1342`, `scan.rs:807` | `src/command/status.rs`, `status/scan.rs`, `status/output.rs` | 同上 | `command::status_test::mutually_referential_symlinks_no_eloop` `(new)` |
| GII-12 | #720; `util.rs:3128` | `src/utils/util.rs`, `rev_parse.rs`, `checkout.rs` | `rev-parse.md`/`checkout.md` + `COMPATIBILITY.md` + 后端 | `command::{rev_parse,checkout}_test::*triple_dot_merge_base` `(new)` |
| GII-13 | #1019; `merge/content.rs:358-423` | `src/command/merge/content.rs` | `merge.md` EN/zh/dev + `COMPATIBILITY.md` + 后端 | `command::merge_test::nested_multiway_conflict_preserves_ours_and_markers` `(new)` |
| GII-14 | #1039; `snapshot.rs:1153` | `src/internal/operation/snapshot.rs` | N/A（内部修复） | `command::op_test::snapshot_symlink_no_false_tree_conflict` `(new)` |
| GII-15 | #875; `cli.rs:2568-2633` | `src/cli.rs`, `src/utils/client_storage.rs` | N/A（内部修复） | `cli::tests::object_index_repair::repair_markers_drain_without_spurious_lbr_io_001` `(new)` |
| GII-16 | #611; `add.rs`, `fsck.rs`, `client_storage.rs` | `src/command/{add,fsck}.rs`, `client_storage.rs` | N/A（内部性能） | `command::{add_test,fsck_test}::*` |
| GII-17 | #656; `agent_architecture_guard.rs:5839` | `src/internal/ai/capture/extraction.rs`, `src/internal/ai/observed_agents/hermes_export.rs` | N/A（内部架构守卫） | `compat_agent_architecture_guard` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划评审通过且 `DEP-GII-01` 满足 | Claude & Codex `PASS` | N/A |
| M1 | Phase 0（`GII-01`..`GII-05`）全部 `done`/`complete` | 各卡 `patch + 1`、`nextest` 与 D 组全绿 | 发布前 revert；发布后按 ER-10 新建 `FIX-*` 前滚发新版本 |
| M2 | Phase 1（`GII-06`..`GII-09`）全部 `done`/`complete` | 同上 | 同上 |
| M3 | Phase 2（`GII-10`..`GII-17`）全部 `done`/`complete` + 全量收口门通过 | 全量 `nextest` + `fmt` + `clippy` 全绿 | 同上 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| `GII-16` 批量 object-index 写入中断 | 未提交事务 | SQLite 事务回滚，索引保持原子一致 | 部分更新导致索引损坏 |
| 远端 D 组 CI 失败 | 已推送版本 tag 与提交 | 按 ER-04/ER-10 新建 `FIX-*` 前滚修复 | 回退已推送提交或删除已发布 tag |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `issues/529.md` 释放 `src/cli.rs` 前抢跑 | 高：合并冲突 | 严格执行 `DEP-GII-01` 门禁 | GII-01..GII-05, GII-15 |
| `GII-17` 误改冻结基线常数 | 高：违反 `ADR-ACF-10` | 仅下沉适配器，不修改 `agent_architecture_guard.rs` 常数 | GII-17 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| `add` / `fsck` 对象哈希与索引 | `O(N_files)` 就地哈希 | `O(1)` 事务批量提交 | `add` 提速 ~25%、`fsck` 提速 ~7%（同机实测） | GII-16 基准测试 |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 对应命令已同步。
- [ ] `docs/commands/{init,status,rev-parse,checkout,merge,README}.md` 及 `zh-CN/` 已同步。
- [ ] `docs/development/commands/{init,status,merge}.md` 已同步。
- [ ] `/Volumes/Sea/libra-backend` 已通过 `sync-libra-command-docs.py --write --only <cmd> --libra-root /Volumes/Data/gitmono/libra` 同步。
- [ ] `docs/error-codes.md`：`N/A`（复用现有错误码）。
- [ ] `tests/INDEX.md` 已核对（仅扩充既有 `command_test` / `compat_*`）。
- [ ] `Cargo.toml` `[[test]]`：`N/A`。
- [ ] `plan-long.md`：`N/A`（由协调者统一维护）。

## Codex review log

| Round | Reviewer | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|---|
| R1 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | 网络中断（`ECONNRESET`，未完成评审） | 无 | R2 重跑 |
| R1 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | P1: 并发写集重叠、ADR-GII-01 预处理边界缺失、GII-16 基准与回滚缺失、测试矩阵 filter 缺失、全局选项后端文档说明缺失、squash merge 签名/tree 核验缺失 | 无 | R2 修订 |
| R2 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R3 重跑 |
| R2 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | P1: `GII-16` 基准缺可复现协议与命令；`ADR-GII-01` 未把 `src/utils/output.rs` 列入写集及缺对应回归用例 | 无 | R3 修订 |
| R3 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R4 重跑 |
| R3 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | P1: `GII-16` 相同文件内容+缺提交身份、`G-04` `landing` 与 `observed_agents/` 目录、`GII-01` 后端文档矛盾 | P2: `GII-01`/`05` 测试说明补全 | R4 修订 |
| R4 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R5 重跑 |
| R4 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | P1: 粒度审计表 `GII-17` 写 `1/1`（卡内为 `2/2`） | 无 | R5 修订 |
| R5 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R6 重跑 |
| R5 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | P1: ① 网站路径 `../libra-backend` 与 `AGENTS.md:135` `/Volumes/Sea/libra-backend` 不一致且缺前端/部署门；② `GII-01`..`04` 全局选项不应豁免网站文档 | 无 | R5 修订 |
| R6 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | 未运行（R5 网络中断后随 R6 修订一并重跑） | 无 | R7 重跑 |
| R6 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | P1: ① `ER-04/08` `git diff` 在 `.libra` 仓库不可用；② `ADR-GII-01` 启动扫描未规定既有全局 flag 与值识别 | P2: 追溯表 `GII-01..03,05` 缺 `index.en.mdx` 及 `GII-17` 目录写法 | R6 修订 |
| R7 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | 未运行（随 R7 修订一并重跑） | 无 | R8 重跑 |
| R7 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | P1: `ADR-GII-01` 误将 `--progress <MODE>` 写为可选值 | 无 | R7 修订 |
| R8 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R9 重跑 |
| R8 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | P1: `GII-12` `rev-parse A...B` 误按唯一 merge-base 而非 Git 对称差 | P2: `GII-01` Verification 明确 `--progress` 组合 | R8 修订 |
| R9 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R10 重跑 |
| R9 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | P1: `GII-12` `rev-parse A...B` 误对无共同祖先/多 merge-base 报错（应允许 0 或全部 `^<mb_i>`，仅单一起点报错） | 无 | R9 修订 |
| R10 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-global-init-infra.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R11 重跑 |
| R10 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-global-init-infra.md` | `PASS` | 无 P0/P1 | 无 | R10 复审通过 |
| R11 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-global-init-infra.md` | `PASS` | 无 P0/P1 | 无 | R11 复审通过 |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-GII-01 | `alias.!<shell>` 外部命令别名任意执行 | 需独立安全沙箱与确认策略 | 明确安全模型后另立计划 | `COMPATIBILITY.md` |

## 完成判据

- [ ] 所有任务卡满足粒度规则 `G-*`；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务 `Lifecycle=done` **且** `Acceptance=complete`（ER-04）。
- [ ] 全部任务卡完成后运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿，且修复全量暴露的全部 Bug（ER-13）。
- [ ] Claude 与 Codex 最终评审结论均为 `PASS`，P0/P1 全部关闭。
- [ ] 每张 `independent` 发布卡已执行 `patch + 1`、版本面集合一致、`Cargo.lock` 仅由工具链刷新、提交推送及 `gh release create` 完成并取得 D 组证据（ER-08）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
