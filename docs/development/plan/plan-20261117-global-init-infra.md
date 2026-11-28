# 全局 CLI、Init 与基础设施修复计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：Claude R26 与 Codex R26 双审 PASS（受审 SHA-256 前 12 位 973afedc5a69），计划可提交；全部任务 pending，尚未执行。
> 审阅合同：按用户 2026-11-18 指令，本计划的计划文本提交门为 Claude（Subagent）与 Codex 双审，二者须对同一受审字节（同一 SHA-256）均给出 `VERDICT: PASS` 方可提交（取代 2026-11-17 的 Codex 单审口径）；网站交付仍须独立的 Claude Code `PASS`，与计划文本提交门互不替代。
> 跟踪范围：GitHub open issues #611、#656、#686、#720、#801、#834、#875、#942–#946、#948、#987、#1014、#1019、#1039（共 17 项，映射为 24 张任务卡：#686 分为 `GII-01` 与 `GII-19`，#801 分为 `GII-02` 与 `GII-24`，#720 分为 `GII-12` 与 `GII-23`，#948 分为 `GII-09`、`GII-22` 与 `GII-20`，#1014 分为 `GII-11` 与 `GII-21`，#611 分为 `GII-16` 与 `GII-18`）。

## 文档职责

- **网站交付路径（已确认）:** 唯一生产网站 checkout 为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支），即本仓库的同级 `../libra-backend`（与模板 ER-06a 默认相邻工作区一致）；依据：用户 2026-11-18 11:55:09 UTC 确认（本批计划的网站文档交付检出即此路径）。`AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`（本机不存在），属已知文档滞后，本批计划不修改 `AGENTS.md`（登记于「与其它计划的关系」）；`AGENTS.md:135` 的交付流程（直接在 `cf` 上工作、保留无关改动、不建分支/PR/worktree、同步检查与前端验证、Claude Code `PASS`、提交推送并核验 Cloudflare 部署）仍适用。2026-11-18 核对：该路径存在、仅含 `.libra`、`libra status --short --branch` 为 `## cf...origin/cf`。写入前须按「文档与网站验收命令」第 2 步重新核验；任一不满足即 STOP，禁止改用其它检出。

本文解决 Libra 全局 CLI 选项与别名（`-C`、`--git-dir`/`--work-tree`、`GIT_DIR`、`--bare`、`alias.*`）、`init` 仓库初始化与格式校验、`status`、`rev-parse`/`checkout` `<rev>...`、合并冲突渲染、`operation snapshot`、cloud object-index 修复队列、`add`/`fsck` 性能与 Hermes extraction 基线等 17 项基础设施与兼容缺口。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收（GC-01）。

### 适用范围

- 全局入口与仓库发现：`src/cli.rs`、`src/main.rs`、`src/utils/util.rs`、`src/internal/config.rs`。
- 命令模块：`src/command/init.rs`、`src/command/status.rs`、`src/command/status/input.rs`、`src/command/status/scan.rs`、`src/command/status/output.rs`、`src/command/rev_parse.rs`、`src/command/checkout.rs`、`src/command/merge/content.rs`、`src/command/mv.rs`、`src/command/add.rs`、`src/command/fsck.rs`、`src/command/fetch.rs`。
- 核心基础设施：`src/utils/output.rs`、`src/internal/operation/snapshot.rs`、`src/utils/client_storage.rs`、`src/internal/ai/capture/extraction.rs`、`src/internal/ai/observed_agents/extract.rs`。
- 测试与文档：`tests/command/{init,status,add,fsck,merge,rev_parse,checkout,mv,config,fetch,cloud}_test.rs`（`command_test`）、各模块 `#[cfg(test)]` 单元测试、`tests/compat/agent_architecture_guard.rs`、`docs/commands/*.md`、`docs/commands/zh-CN/*.md`、`docs/development/commands/*.md`、`COMPATIBILITY.md` 及 `/Volumes/Data/gitmono/libra-backend` 对应同步文档。

### 非目标

- 不更改 SQLite 数据库 schema 或新增 migration（GC-13 无新表变更；`init` 仅校验 `repositoryformatversion`/`extensions.*`/`core.objectformat` 配置键值）。
- 不更改 `compat_agent_architecture_guard` 冻结的 `ADR-ACF-10` 阈值（#656 的收口只能下沉 Hermes 专用实现，`extraction.rs` 非测试 `AgentKind::` 计数保持 `≤ 15`，不得放宽守卫）。

### 成功定义

- 17 个关联 issue 全部在「追溯表」与任务卡中闭环（各卡 `Out of scope` 明确排除的子项——#611 第 3 项、#943 验收 6、#1039 中 #47/#48 的条件项——按该卡规定另行跟踪或经 ER-03 修订本卡；执行期正式延后的卡，其 issue 按「非目标与延后项」登记为部分闭环、不关闭），对应行为、性能与架构守卫测试通过。
- 受影响的 EN/zh/dev 命令文档、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend` 同步更新。
- 全部非延后任务卡完成后通过 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 及 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

## 使用规则

- 每次状态推进与「修订历史」写入都记录实际 `YYYY-MM-DD HH:MM:SS UTC` 时间。
- 推进或完成任务卡时，执行者须在同一变更中同步更新 [`plan-status.md`](plan-status.md)（本计划入库时的初始登记行由协调者随本批计划合入一并登记）。
- 本文全部任务卡均未开工；任务 ID（`GII-01`..`GII-24`）固定，拆分时按 G-09 在所属 Phase 末尾追加新编号并登记「修订历史」（2026-11-18：`GII-18` 自 `GII-16`、`GII-19` 自 `GII-01`、`GII-20` 与 `GII-22` 自 `GII-09`、`GII-21` 自 `GII-11`、`GII-23` 自 `GII-12`、`GII-24` 自 `GII-02` 拆出）。
- 本计划不单独新建证据目录或证据文件（v2.12），所有门禁结果、SHA 与测试摘要写在本计划内。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。以下锚点于 `2026-11-18`（R1 修订时，`Cargo.toml` = `0.30.210`）重新核对；首稿锚点（`2026-11-17 03:45:00 UTC`，`0.30.207`）已被替换。「已安装 0.30.207 实测」指以本机已安装的 `libra 0.30.207`（与首稿基线同版本）在仓库外临时目录中的复现，不读写本仓库。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 全局 CLI | `src/cli.rs`（5445 行）：全局参数定义于 `Cli`（无 `-C`、`--git-dir`、`--work-tree`、`--bare`）；`status` 的 clap 内置别名 `st`；仓库打开时读取 `core.objectformat` 设定哈希算法；解析错误分类；object-index 修复预检 | `src/cli.rs:95-170,234-336,355,2568-2638,3176-3230,3688,4739` |
| 输出模式 | `OutputConfig::resolve_from_argv` 直接扫描原始 argv 决定 JSON/机器输出 | `src/utils/output.rs:250-327` |
| 仓库发现 | `try_get_storage_path` / `storage_path` / `working_dir` 从 cwd 向上查找 `.libra`，不读 `GIT_DIR` | `src/utils/util.rs:1262-1269,1464` |
| `init` | `--template`（缺失目录报 `InitError`）、`--shared`（`SharedMode::parse` 与目录执行位计算）、`--initial-branch` 重初始化告警、`core.objectformat` 校验、新库写 `repositoryformatversion=0` | `src/command/init.rs:105,133,197-200,336-366,720-775,1327-1426,1816-1833,2373-2378`、`src/utils/shared_perm.rs:32-60,205-237` |
| `status` 与修订 | `StatusArgs` 无 `-v`；扫描用 `symlink_metadata`，检查失败告警 `cannot inspect` 并以输出不完整退出；`get_commit_base_typed` 不识别 `...` | `src/command/status/input.rs:27`、`src/command/status.rs:745,1342-1537`、`src/command/status/scan.rs:443,600,807`、`src/command/status/output.rs:1303`、`src/utils/util.rs:3128` |
| 合并、快照与 `mv` | 外层冲突标记长度 = `unambiguous_conflict_marker_length` + `extra_marker_size`；snapshot 工作区扫描 `scan_working_copy` 的 I/O 错误映射为 `working-copy scan failed`；`insert_tree_path` 文件/目录冲突；`mv` 未检查目标前导符号链接（已安装 0.30.207 复现 #1039） | `src/command/merge/content.rs:358-363,954`、`src/internal/operation/snapshot.rs:106-115,256-340,1131-1157`、`src/command/mv.rs:117-120,236-252` |
| add / fsck / fetch | `fsck::verify_object` 先拷贝 header+data 再求摘要；`git_object_hash` 已就地；`add` 批末持久化 marker 后逐对象入队，后台逐条提交；批量回放 `repair_pending_object_index_updates`；`fetch` 的存储解析失败一律报 `StorageConfig`（`LBR-IO-001`）；命令后台排空预算 60 秒 | `src/command/fsck.rs:2388,2489-2498`、`src/utils/object.rs:1062-1072`、`src/utils/client_storage.rs:1021-1066,1793-1800,2203-2215`、`src/command/fetch.rs:1098-1099,2547-2550`、`src/cli.rs:4208-4232` |
| Agent 守卫 | 冻结 `("src/internal/ai/capture/extraction.rs", 15)`（`ADR-ACF-10`，只许下降）；oracle 冻结守卫；`extraction.rs` 当前非测试 `AgentKind::` 路径 14 处，Hermes 以字符串比较分派 | `tests/compat/agent_architecture_guard.rs:5846-5859,9278-9285`、`src/internal/ai/capture/extraction.rs:421-422,457-473` |
| 机器输出 | 各命令 `--json` 保持向后兼容，无新增 JSON schema 破坏 | `src/command/{init,status,add,fsck}.rs` |
| 错误码 | 复用 `LBR-CLI-001`（129）、`LBR-CLI-002`（129）、`LBR-CLI-003`（129）、`LBR-REPO-001`（128）、`LBR-REPO-002`（128）、`LBR-REPO-003`（128）、`LBR-IO-001`（128）、`LBR-IO-002`（128），不新增 | `docs/error-codes.md:97-102,119-120`、`src/utils/error.rs:373-392` |
| 文档 | 中英文命令页、开发文档及 `COMPATIBILITY.md` 已存在；网站交付路径与流程见「文档职责」与「文档与网站验收命令」：子命令页由 `/Volumes/Data/gitmono/libra-backend/scripts/sync-libra-command-docs.py`（`--check` / `--write` / `--only`）同步，全局 CLI 选项同步 `apps/tanstack-app/content/docs/commands/index.en.mdx` 的 `## Global Flags`（第 21 行） | `docs/commands/README.md:18`、`docs/commands/zh-CN/README.md:14`、`COMPATIBILITY.md:165` |
| 测试 | `tests/command/*_test.rs` 挂载于 `command_test`（测试名形如 `command::<module>_test::<fn>`）；单元测试名形如 `cli::tests::…`、`utils::util::test::…`、`utils::client_storage::tests::…`、`command::fsck::tests::…`、`internal::operation::snapshot::…`、`internal::ai::observed_agents::extract::tests::…`、`internal::ai::capture::extraction::tests::…`；compat：`compat_agent_architecture_guard`、`compat_rev_parse_peel_selectors` | `tests/command_test.rs:10`、`tests/command/mod.rs:988-1145`、`src/cli.rs:4256`、`src/utils/util.rs:4214-4215`、`src/utils/client_storage.rs:4532`、`src/command/fsck.rs:3036`、`src/internal/operation/snapshot.rs:1230-1240`、`src/internal/ai/observed_agents/extract.rs:1809`、`src/internal/ai/capture/extraction.rs:837`、`tests/INDEX.md:69,75` |
| 外部参照 | 上游 Git 测试树 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（v2.56.0-116；t0001、t2018#4、t3200#95、t4030#18、t6415#23、t7001） | 各 issue 正文（2026-11-18 以 `gh issue view` 核对） |
| 已不复现 / 已复现 | 已不复现：#943（裸 / 非裸 `init --shared=0666` 均退出 0，目录 `777`、文件 `666`、`vault.db` `600`）、#656（`extraction.rs` 计数 14 ≤ 15）；已复现：#1039（`mv` 越过符号链接后快照树冲突）、#1014 夹具需「被替换的已跟踪目录」才复现 | `GII-07`、`GII-11`、`GII-14`、`GII-17` 的 `Current evidence` |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-GII-01 | `#686`：全局拒绝 `-C <path>`，导致 t3200#95 等工作流中止 | 无法在子目录/他处切换工作目录运行命令 | `src/cli.rs`、`src/main.rs` | GII-01 |
| GAP-GII-02 | `#801`：全局拒绝 `--git-dir` / `--work-tree` | 无法通过全局参数指定仓库与工作树 | `src/cli.rs`、`src/main.rs` | GII-02、GII-24 |
| GAP-GII-03 | `#987`：忽略 `GIT_DIR`（及 `GIT_WORK_TREE`）环境变量并静默操作 cwd 仓库 | 外部脚本/Git 兼容层指向错误仓库 | `src/cli.rs`、`src/main.rs`、`src/utils/util.rs` | GII-03 |
| GAP-GII-04 | `#944`：全局拒绝 `--bare`（如 `libra --bare init` 致命失败） | 与 Git 全局 `--bare` 语法不兼容 | `src/cli.rs`、`src/command/init.rs` | GII-04 |
| GAP-GII-05 | `#945`：不展开 `alias.*` 配置（`libra <alias>` 被拒绝） | 用户自定义 Git 别名不可用 | `src/cli.rs`、`src/main.rs`、`src/internal/config.rs` | GII-05 |
| GAP-GII-06 | `#942`：`init --template` 在模板目录不存在时致命报错而非告警后继续建库 | 与 Git 容错语义不一致 | `src/command/init.rs:105,197-200,722-726` | GII-06 |
| GAP-GII-07 | `#943`：报告 `init --shared=0666` 被拒；当前源码与已安装 0.30.207 已接受并补目录执行位，但无 `0666` 回归用例与文档示例 | 回归无守卫，#943 未收口 | `src/utils/shared_perm.rs:32-60,205-237`、`tests/command/init_test.rs`（无 `0666`） | GII-07 |
| GAP-GII-08 | `#946`：`init` 重初始化告警输出 `"ignoring --initial-branch"` 而非 `"ignored --initial-branch"` | 与 Git 告警文案及上游用例断言不符 | `src/command/init.rs:1426` | GII-08 |
| GAP-GII-09 | `#948`：仓库打开缺少 `repositoryformatversion` 与 `extensions.*` 校验（版本 0 + v1-only 扩展、未知扩展、非法 `extensions.objectformat` 均被静默接受） | 未知扩展或格式版本未 fail-closed | `src/cli.rs:95-170`、`src/command/init.rs:133,1327-1413,1816-1833,2373-2378` | GII-09、GII-22、GII-20 |
| GAP-GII-10 | `#834`：`status` 拒绝 `-v`/`--verbose`（t4030 中途失败） | 无法在 `status` 展示已暂存差异 | `src/command/status/input.rs:27` | GII-10 |
| GAP-GII-11 | `#1014`：已跟踪目录被替换为互指符号链接后，检查已跟踪路径时解析前导符号链接分量触发 `ELOOP`，`status` 与 operation snapshot 扫描致命失败 | `status`、`add`、`commit`、`reset`、`checkout`、`merge` 均不可用 | `src/command/status/scan.rs:443,600`、`src/command/status.rs:745,1342`、`src/internal/operation/snapshot.rs:106-115,337` | GII-11、GII-21 |
| GAP-GII-12 | `#720`：不解析 `<rev>...`（三点 merge-base 语法，如 `checkout -b` 起点与 `rev-parse`） | 对称差/merge-base 语法失败 | `src/utils/util.rs:3128`、`src/command/{rev_parse,checkout}.rs` | GII-12、GII-23 |
| GAP-GII-13 | `#1019`：嵌套多路冲突渲染将外层标记加宽至 9 且丢弃 `ours` 区域一行 | 嵌套冲突内容丢失且标记宽度异常 | `src/command/merge/content.rs:358-363,954` | GII-13 |
| GAP-GII-14 | `#1039`：`mv` 未拒绝越过符号链接的目标，把 index 改写为 `from/link` 与 `from/link/...` 并存，随后快照报 `tree path conflicts with file 'link'` 并阻断 `reset` 等命令 | 仓库被 index 同名冲突卡死 | `src/command/mv.rs`、`src/internal/operation/snapshot.rs:1131-1157` | GII-14 |
| GAP-GII-15 | `#875`：repair marker 积压期间本地 `fetch` 间歇性被误报为存储配置错误（`LBR-IO-001`） | 本地 `fetch` 偶发失败 | `src/command/fetch.rs:1098-1099,2547-2550`、`src/utils/client_storage.rs:1793-1800`、`src/cli.rs:4208-4232` | GII-15 |
| GAP-GII-16 | `#611` 第 2 项：`add` 批末逐对象入队、逐条提交 object-index 更新 | 每文件额外的数据库打开与 `fsync`，`add` 偏慢 | `src/utils/client_storage.rs:1021-1066` | GII-16 |
| GAP-GII-17 | `#656`：Hermes extraction 曾使 `AgentKind::` 计数超出冻结基线 15；当前计数 14，但以 `"hermes"` 字符串分派并在 `extraction.rs` 内保留 Hermes 专用实现，#656 未收口 | 与 `ADR-GII-02` 的 provider-neutral 目标不符 | `src/internal/ai/capture/extraction.rs:421-422,457-473`、`tests/compat/agent_architecture_guard.rs:5846-5859` | GII-17 |
| GAP-GII-18 | `#611` 第 1 项：`fsck::verify_object` 拷贝 header+data 后再求摘要 | `fsck` 每对象多一次整对象拷贝 | `src/command/fsck.rs:2489-2498` | GII-18 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `issues/529.md` | 当前持有 `src/cli.rs` 写锁 | 通过 `DEP-GII-01` 等待 `issues/529#FP-13` 释放 `src/cli.rs` 写锁后再启动 `GII-01`；其后触及 `src/cli.rs` 的 `GII-19`、`GII-02`、`GII-24`、`GII-03`..`GII-05`、`GII-09`、`GII-22`、`GII-20`、`GII-15` 经串行链传递满足 |
| 同批 `plan-20261117-*.md` 计划 | 与本计划多张卡的 `Implementation write set` 相交：`COMPATIBILITY.md` 为全部同批计划共享；2026-11-18 核对的源码与文档交集例如 `src/cli.rs`、`src/utils/util.rs`、`src/internal/config.rs`、`src/command/{init,status,rev_parse,checkout,fetch,add}.rs`、`tests/command/init_test.rs` 与对应命令文档、网站页（以开工当日 `plan-status.md` 与各计划写集为准） | 通过 `DEP-GII-03`：每张卡开工编辑前核对与同批在途卡的写集交集，有交集时取得协调者串行窗口（G-10）；只按计划名与卡 ID 引用，不修改其它计划 |
| `AGENTS.md:135` | 网站交付常设指令（2026-11-18 仍写 `/Volumes/Sea/libra-backend`，本机不存在） | 已知文档滞后：本批网站交付检出以用户 2026-11-18 11:55:09 UTC 确认的 `/Volumes/Data/gitmono/libra-backend` 为准；本计划不修改 `AGENTS.md` |
| `plan-long.md` | 长期基础设施与兼容路线 | 本计划为具体执行落地，不直接改写 `plan-long.md` |
| `COMPATIBILITY.md` | 对应命令兼容矩阵 | 原地更新对应命令单元格，不破坏锚定行号 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 17 项 issue 覆盖全局参数、初始化、状态、冲突、快照、性能与 Agent 基线 | 每项独立成单轴卡 `GII-01`..`GII-17`，并按恢复轴拆出 `GII-19`（#686）、`GII-20` 与 `GII-22`（#948）、`GII-21`（#1014）、`GII-18`（#611）、`GII-23`（#720）与 `GII-24`（#801）；`GII-07`（#943 已不复现）为 `audit` 卡 |
| 可行性 | 全局参数/修复队列集中于 `cli.rs`/`main.rs`，`init` 集中于 `init.rs` | 同文件写集严格串行，无写集冲突卡可按 DAG 推进 |
| 任务卡粒度 | 24 张卡均为单轴，按独立谓词计数（`implementation` 卡 `AC ≤ 8`、`VER ≤ 8`；`audit` 卡 ≤ 20），`scope ∈ {S, M}`，含公开行为变更的卡均为 `M` | 无 L/XL 卡，无多轴卡 |
| 依赖与顺序 | `DEP-GII-01` 门控 `src/cli.rs`；与同批计划相交的写集经 `DEP-GII-03` 串行；计划内共享写集（`cli.rs`、`init.rs`、`status.rs`、`util.rs`、`snapshot.rs`、`client_storage.rs`、`merge_test.rs`）均已串行 | 无环、无 I–I 并发冲突 |
| 完整性 | 每卡含精确 `nextest` filter（经 `cargo nextest list` 核对）、AC → 断言映射、ER-13 `T-1` 全量触发、D 组真实 CI job、文档与兼容同步 | 符合 ER-04、ER-06/ER-06a、ER-08、ER-13、ER-14 |
| 安全性 | `alias.*` 防循环展开、`-C`/`--git-dir`/`GIT_DIR` 路径边界、`extensions.*` fail-closed | 继承 GC-07、GC-11 |
| 功能正确性 | 对照 `git@6de20f6092`（t3200#95、t4030）与 `ADR-ACF-10` 冻结基线 | 判据精确可测 |
| 接口兼容 | `GII-09` 严格校验 `repositoryformatversion` 与 `extensions.*` | 记录兼容窗口 |
| 数据流与控制流 | `GII-15` 存储解析与排空竞态、`GII-16` 批量回放事务、`GII-14` `mv` 不再写出同名冲突的 index、`GII-11` 扫描不经前导符号链接 | 继承 GC-08 |
| 性能与容量 | `GII-16` / `GII-18` 固定基准阈值（`add` 中位降幅 `≥ 10%`、`fsck` `≥ 5%`；#611 报告约 25% / 7% 含已落地的就地哈希收益） | 继承 GC-10 |
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
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（用户最新指令指定；`AGENTS.md` 的 `/Volumes/Sea/libra-backend` 已被取代且本机不存在），并在「文档职责」写明权威依据与 STOP 条件【R8 更正：本行所写「用户最新指令指定」「已被取代」为当时的转述，未附确认原文；2026-11-18 核对 `AGENTS.md:135` 仍指定 `/Volumes/Sea/libra-backend`。路径替换的授权不因本行生效，以 `DEP-GII-02` 满足为准（见「文档职责」与依赖登记表）】【R9 补记：用户 2026-11-18 11:55:09 UTC 确认本批网站文档交付检出为 `/Volumes/Data/gitmono/libra-backend`（`cf`），`DEP-GII-02` 据此已满足；`AGENTS.md:135` 的差异登记为已知文档滞后】 | N/A | 全文对应章节 |
| 时间不可考（最迟于 2026-11-18 10:11:04 UTC 已存在；R13 更正：上界不作事件时间） | Codex 复审 FAIL 修复（验证契约与门禁）【R1 更正：原记时间 `2026-11-18 10:35:00 UTC` 晚于实际写入时刻（本行最迟于 10:11:04 UTC 已存在于文件中），属未来时间；实际写入时刻不可考，改记该可证上界】 | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 【R1 更正：本行所述「模块过滤统一为 `test(/^X::/)`」「文档与网站验收命令」块、「家族子卡只做本地门、唯一发布点统一推送、部署绑定 `$FINAL`」在当时的字节中并未落地（本计划无家族卡），AC → 断言映射为套话；上述缺陷由 R1 修订行补正】 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 10:12:44 UTC | 用户 2026-11-18 指令（计划文本提交门改为双审） | 将文头「审阅合同」与「完成判据」中的计划文本提交门由 Codex 单审改为 Claude（Subagent）与 Codex 双审、二者对同一受审 SHA-256 均 `VERDICT: PASS` 方可提交；执行期各卡 ER-05 评审门与网站交付 Claude Code `PASS` 门不变 | N/A | 文头、完成判据 |
| 2026-11-18 10:49:30 UTC | 双审 R1：Claude FAIL（18×P1、5×P2）、Codex FAIL（10×P1），受审 SHA-256 `51d0a469d255…` | ① 重写「验证执行协议」：卡特有用例 / 共享回归组 / 非 nextest 门三分类，先 `cargo nextest list` 核对精确集合，判据改为退出码 0、`F=0`、`N=P=列出条数`（nextest 将过滤排除计入 `skipped`，不再要求 `S=0`）；② 新增「文档与网站验收命令」7 步（逐文件 SHA-256、`cf` 前置、同步 `--check`/`--write`、`typecheck`/`build`、Claude Code `PASS`、提交树与受审字节核对、`ls-remote` 与有界部署核验）；③ 全部卡重写 AC 为独立谓词并按 G-03 重算计数，「AC → 断言映射」改为逐场景的退出码、输出与前后状态；共享回归过滤改为锚定正则（原 `test(command::init_test::template)` 等零匹配）、单元测试名补全 crate 内路径（`internal::…::tests::`、`utils::util::test::`）、测试矩阵去除 `\|` 改用 `or`；④ 按 GC-01 核对源码与已安装 0.30.207：`GII-07`（#943 已不复现）改为回归守卫收口卡，`GII-17`（计数已为 14）改为把 Hermes 字符串分派与实现下沉到 `observed_agents/extract.rs`，`GII-11` 夹具改为 #1014 真实复现并纳入 snapshot 扫描，`GII-09` 补 #948 的版本 0 + v1-only 扩展与非法值规则（`LBR-REPO-002`、写集加 `src/cli.rs` 预检），`GII-10` 写集改为 `status/input.rs`，`GII-05` 示例避开内置别名 `st`，`GII-02` 写集补 `util.rs`；⑤ `GII-16` 前提更正（`git_object_hash` 已就地，`add` 为批末逐对象入队），按 G-01/G-09 拆出 `GII-18`（`fsck` 就地哈希），阈值改为 `add ≥ 10%`、`fsck ≥ 5%`，故障注入改为单元测试；⑥ 事实基线锚点刷新至 `0.30.210`，`AGENTS.md:135` 表述改为「流程适用、路径被取代」，恢复 R5 修订历史与 Review log 原文（`/Volumes/Sea/libra-backend`），更正 10:35 未来时间行；⑦ 追溯表恢复精确 `(new)` 测试与网站文件，Phase 1 进入条件改为 `GII-05`，完成判据补模板条目 | `GII-16` → `GII-16`, `GII-18` | 文头、事实基线、当前缺口、ADR-GII-02、GC-05、执行检查必备需求、实施顺序、Phase、审计表、全部任务卡、测试矩阵、追溯表、里程碑、风险、性能摘要、兼容收口、完成判据、Review log |
| 2026-11-18 10:52:19 UTC | 双审 R2：Claude FAIL（4×P1、4×P2）、Codex FAIL（受审期间文件被修订，未能对 `a1323b5c7232…` 给出结论） | ① 恢复 18 处 `---` 分隔线前的空行（缺空行使各卡 `Granularity` 行被渲染为 setext 标题）；② `GII-09` 增加 AC7：非法格式状态下 `libra config` 仍可读写以修复（Libra 配置存于 SQLite 的有意差异，写入文档），`docs/error-codes.md` 统一为 `N/A`，AC 计数 6→7；③ `GII-02` AC6 缺值用例改为 `--work-tree=`（原 `--work-tree status` 会把 `status` 当路径值）；④ `ADR-GII-01` 别名查找排除 clap 内置别名并更正 `output.rs:250-327`；⑤ 非目标措辞、`GII-03` 夹具 `<s>`、GAP 行序、`DEP-GII-01` 复核日期与基线「临时目录复现」措辞。流程改正：此后每轮在两位评审完成前冻结文件，不再在受审期间编辑 | N/A | ADR-GII-01、非目标、事实基线、当前缺口、依赖登记表、审计表、GII-02/03/09、全部卡分隔线、Review log |
| 2026-11-18 11:06:22 UTC | 双审 R3：Claude FAIL（4×P1、1×P2）、Codex FAIL（10×P1），受审 SHA-256 `8069ec0159ed…` | ① 明确 G-03 计数口径（一条 AC 一个谓词与一个规范场景，等价输入与判定细节写入映射，不同形式/结果/触发条件拆开，「附加断言」不计）并据此重写 `GII-01/02/03/04/05/07/09/11/12/14/15/16/17` 的 AC；② `ADR-GII-01`/`GII-01`：`-C ''` 改为 Git 的不切换目录，映射基线改为写操作前的 `status` 快照；③ ER-04/08 squash 核验改为比较 `^{tree}` 对象 ID（禁用位置参数 `libra diff`）；④「验证执行协议」新增 3a 压力门判据；「文档与网站验收命令」第 2 步改为 `test -d .libra && test ! -e .git`，第 7 步以推送前不存在、推送后出现的唯一标记文本绑定部署；⑤ `GII-07` 补裸仓库 `--bare --shared=0666`（#943 验收 1–5，验收 6 另行跟踪）；⑥ `GII-09` 新增 `extensions.refstorage` 非法值 AC 并对每个非法状态做 `libra config` 修复检查；⑦ `GII-11` 根因改为前导符号链接分量解析（`has_symlink_leading_path` 语义），补 `checkout`、`t6415` #23 合并与缺失目标链接；⑧ `GII-14` 经已安装 0.30.207 复现确认 #1039 根因为 `mv` 未拒绝越过符号链接的目标，改为 `mv` 修复卡并纳入文档门（`GII-01`..`GII-14`）；⑨ `GII-15` 改按真实失败路径（`fetch` 的 `StorageConfig` 误判、`cloud` 为 `LBR-IO-002`）重写，加 tc1375 循环复现，写集加 `fetch.rs`，scope 改 `M`；⑩ `GII-16/18` 基准要求基线与本卡二进制分置于 `base/`、`card/` 且仍名为 `libra`，脚本不吞 stderr；追溯表、测试矩阵、审计表、事实基线与当前缺口同步 | N/A | 文头、事实基线、当前缺口、ADR-GII-01、GC-05、执行检查必备需求、任务卡粒度规则、默认覆盖、审计表、GII-01/02/03/04/05/07/09/11/12/14/15/16/17/18、测试矩阵、追溯表、兼容收口、Review log |
| 2026-11-18 11:15:42 UTC | 双审 R4：Claude FAIL（1×P1、2×P2）、Codex FAIL（6×P1），受审 SHA-256 `928e3021aeb0…` | ① G-03 计数改为严格口径：删除全部「附加断言」，映射只写判定对应 AC 的检查，可独立失败的断言一律成为计数 AC；据此 `GII-01` 按 G-09 拆出 `GII-19`（`-C` 与既有全局标志交错及输出模式，追加于 Phase 0，链改为 `GII-01 -> GII-19 -> GII-02`），`GII-04`（布局 / `core.bare` 拆开）、`GII-07`（裸 / 非裸各 4 条，`0400` 交由共享回归组）、`GII-10`（`-v`/`-vv`/`--verbose`/只读拆开）、`GII-11`（合并成功与结果拆开）重写 AC，`GII-02/03/05/06/08/09/14` 去除映射中的额外断言并收窄为单一规范场景（`GII-09` AC8 只承诺版本 > 1 状态下的修复通道）；②「文档与网站验收命令」增加网站无变更分支（`--check` 首次退出 0 且无手工修改时记录 SHA-256 后跳过第 5–7 步），第 7 步零命中改按退出码模板并单独下载页面快照（`curl` 失败即 STOP）；③ `GII-17` AC6 差异门改为提交前以工作区为新侧、提交后复核；④ `GII-15` 更正源码归因（`LIBRA_STORAGE_TYPE` 来自 `read_storage_type` 读取仓库数据库失败，`StorageOpenError::Io` 为 `objects`），新增仓库数据库持锁 2 秒的确定性单元与端到端用例，原预检积压用例只保留 `cloud` fail-closed 守卫，scope 改回 `S`；⑤ `GII-11` 缺失目标改为「已跟踪目录被替换为悬空符号链接」；⑥ 成功定义注明各卡 Out of scope 子项；审计表、追溯表、Phase、实施顺序、风险与里程碑同步 `GII-19` | `GII-01` → `GII-01`, `GII-19` | 文头、成功定义、使用规则、与其它计划的关系、评审结论、ADR-GII-01、GC-05、执行检查必备需求、实施顺序、发布分组、Phase 0/2、任务卡粒度规则、审计表、GII-01/02/03/04/05/06/07/08/09/10/11/14/15/17/19、追溯表、里程碑、风险、Review log |
| 2026-11-18 11:22:21 UTC | 双审 R5：Claude FAIL（1×P1、1×P2）、Codex FAIL（6×P1），受审 SHA-256 `b9751f0b14e0…` | ① `GII-01`/`GII-19` 重划边界：`-C` 路径值不触发输出标志（`-C --json`）回到 `GII-01`（本卡发布即自洽，交错形式按现状拒绝并在文档写明限制），`GII-19` 改为既有全局标志交错与 `--` 透传；`GII-01` AC4 改用名为 `-C` 的未跟踪文件证明其作为 pathspec 处理；② `GII-15` 加确定性握手与环境隔离：`env -u LIBRA_STORAGE_TYPE` 运行、用例断言变量未设置，单元用例以修复引入的忙状态计数握手，端到端用例改用仅 debug + `LIBRA_TEST=1` 生效的故障注入点 `LIBRA_TEST_STORAGE_TYPE_READ_LOCK_MS` 并以 stderr 生效行证明命中目标读取；③ 网站无变更分支在 GC-05、兼容收口与完成判据中一致化；④ 继续按独立谓词收紧：`GII-02` 去掉与 `GII-19` 重复的 `--` 透传 AC，`GII-06` AC4、`GII-12` AC6–AC8、`GII-14` 去除映射中的额外检查（`GII-14` 两种插入顺序拆为 AC5/AC6），`GII-10` 只读拆为 `HEAD` / index 两条，`GII-18` 把 `HashMismatch` 升为 AC3；⑤ `VER` 分子只计卡特有门，共享回归组标注「不计入 G-03 VER」并重算全部 `Granularity` 与审计表；⑥ 新增「测试串行登记」约束（新增 `#[serial]` 用例时 `tests/SERIAL_REGISTRY.tsv` 与 `.config/nextest.toml` 并入该卡写集并触发 `T-4`） | N/A | 全局工程约束、执行检查必备需求、完成判据、兼容收口、审计表、GII-01/02/06/10/12/14/15/18/19 与全部卡 `Verification`、追溯表、Review log |
| 2026-11-18 11:31:51 UTC | 双审 R6：Claude PASS（3×P2；读到 Codex 结果后补记漏判 6 项）、Codex FAIL（6×P1），受审 SHA-256 `bec2834230bc…` | ① 网站无变更分支改为同时要求目标文件无未提交改动、本地 `cf` 等于 `origin/cf`、`--check` 首次退出 0、线上页面已含本卡关键词，目标文件有他人改动即 STOP；② `GII-15` 插桩（`#[cfg(test)]` 原子计数）与行为修复分步写入，先在未修复树上复现失败，握手最多等 10 秒、锁由守卫释放、`join` 30 秒上限；③ `GII-09` 按 G-09 拆出 `GII-20`（`extensions.refstorage`），`GII-09` 修复通道改为覆盖全部非法状态的矩阵 AC，并补 `config` 命令文档与网站页；④ `GII-08`、`GII-13`、`GII-14` 因公开行为变化改为 `M`；⑤ `GII-07` 改为 `audit` / `no-release` 卡（只读复现 #943 验收 1–5 并补文档，C/D 覆盖继承 `GII-08`，不再新增仅补测试的代码改动）；⑥ `GII-11` 按 G-09 拆出 `GII-21`（operation snapshot 扫描），两卡各加「有效目标的前导符号链接不被穿透」的可观察 AC，删去无法观察的「残余 `ELOOP` 告警」承诺；⑦ G-03 矩阵例外的措辞覆盖修复通道矩阵；⑧ 补正 ADR-GII-01 结果段、`GII-14` AC3 错误码与风险登记的过期 AC 引用；审计表改由各卡 `Granularity` 生成，实施顺序、Phase、里程碑、追溯表与当前缺口同步 21 张卡 | `GII-09` → `GII-09`, `GII-20`；`GII-11` → `GII-11`, `GII-21` | 文头、使用规则、当前缺口、与其它计划的关系、评审结论、ADR-GII-01、GC-05、执行检查必备需求、实施顺序、Phase 1/2、默认覆盖、任务卡粒度规则、审计表、GII-07/08/09/10/11/12/13/14/15/20/21、追溯表、里程碑、风险、Review log |
| 2026-11-18 11:46:56 UTC | 双审 R7：Claude FAIL（1×P1、1×P2）、Codex FAIL（6×P1），受审 SHA-256 `38551546341e…` | ① `GII-07 -> GII-08` 承载例外：`GII-07` 达 `locally-accepted` 且 ER-05 `PASS` 即满足 `GII-08` 前驱，其仓库改动随 `GII-08` 的 C 组提交，两卡随 `GII-08` 的 C/D 同步进入 `remote-pending` / `complete`；② 网站第 2 步改为所有分支的前置（`DEP-GII-02`、目标文件无他人改动、`libra fetch` 后本地 `cf` 与 `origin/cf` 对账：仅落后则 `pull --ff-only`，超前或分叉即 STOP），第 3 步按目标文件是否变化判定分支，无变更分支以已提交内容与线上页面中的本卡行为句 `B` 核验，并新增不建提交的部署恢复分支；`M` / `B` 限纯文本句；推送被拒即 STOP；③ 登记 `DEP-GII-02`（网站路径替换须用户确认原文入档或 `AGENTS.md:135` 对齐），`GII-01`、Phase 0 与 M0 依赖之，「文档职责」改为待确认；④ 删除 G-03「矩阵 AC」例外：`GII-09` 按 G-09 拆出 `GII-22`（`extensions.objectformat` 取值与一致性），`GII-09`、`GII-20`、`GII-22` 的修复通道按每个非法状态各计一条 AC，`GII-18` 原 AC1 按对象格式与空对象拆为四条；并更正 `GII-09`「`libra config` 不经预检」的错误前提（实为 `src/cli.rs:1889` 仅豁免 `--global` / `--system` / schema doctor，本地 `config` 在 `src/cli.rs:3687` 经预检），改为显式豁免 `Commands::Config`；⑤ `GII-07` B 组门改为只比较本卡新引入的改动并允许协调文件，基线无关改动保持原样，并按模板 `audit` 类型门补「内部链接与 `file:line` 锚点可解析」；⑥ 测试矩阵集成行补 `command::op_test`（`GII-21`）；⑦ `GII-07` AC11 拆为 AC11 / AC12；同步审计表、实施顺序、Phase、里程碑、追溯表、当前缺口、风险登记、跨计划关系与评审结论 | `GII-09` → `GII-09`, `GII-22` | 文头、文档职责、使用规则、全局工程约束、执行检查必备需求、实施顺序、依赖登记表、粒度规则、字段默认、审计表、GII-01、GII-07、GII-08、GII-09、GII-18、GII-20、GII-22、测试矩阵、追溯表、里程碑、风险登记、与其它计划的关系、评审结论、Review log |
| 2026-11-18 11:54:12 UTC | 双审 R8：Claude PASS（2×P2）、Codex FAIL（5×P1），受审 SHA-256 `f5c003050eac…` | ① 网站第 2 步增加「暂存区为空」前置（`libra status --porcelain` 首列；Libra 提交整个暂存区，`src/command/commit.rs:253-255`），第 6 步核对暂存路径集合与提交文件集合（`libra show --name-only --format=format: HEAD`）恰为本卡变更文件；② 第 3 步改为逐文件判定：变更文件走 `M` 与第 4–7 步，未变更文件各自以已提交内容与线上页面中的行为句 `W` 核验（含部署恢复路径），全部未变更时为无变更分支；行为句改名 `W` 以免与卡内基线占位符 `B` 混淆；③ `GII-07` 补计本卡特有的文档语义判据 AC13–AC17（三份命令/开发文档、`COMPATIBILITY.md`、网站页）及 5 项手工核对，`AC=17/20`、`VER=7/20`；④ `GII-07` 改为 `compensating`：本仓库改动形成独立本地签名提交（承载例外同步要求精确暂存与提交文件集合核对），补偿动作分别撤销本仓库提交（合入前 `revert`，合入后 `FIX-*` 前滚）与网站提交（`cf` 上 revert 并核验部署），幂等键为两个提交 SHA；⑤ 2026-11-18 08:19:08 UTC 行保留原文并追加 R8 更正：路径授权未经核实，以 `DEP-GII-02` 为准；⑥ 顺带修正 Claude R8 的 2 项 P2（`GII-09` 证据锚点补 `init.rs:2399`；`GII-22` AC1 映射措辞）；同步审计表、字段默认覆盖、`GII-08` 依赖说明、完成判据与兼容收口措辞 | N/A | 文头、修订历史（08:19:08 行更正）、全局工程约束、执行检查必备需求、实施顺序、字段默认、审计表、GII-07、GII-08、GII-09、GII-22、兼容与文档收口、完成判据、Review log |
| 2026-11-18 12:00:10 UTC | 双审 R9：Claude FAIL（1×P1、2×P2）、Codex FAIL（4×P1），受审 SHA-256 `51af0fcc286f…` | ① 落实用户 2026-11-18 11:55:09 UTC 确认：网站检出 `/Volumes/Data/gitmono/libra-backend`（`cf`）为已确认依据，`DEP-GII-02` 改记为已满足且不再是任何前置条件（移出 `GII-01` 依赖、Phase 0 进入条件、串行链、M0 与网站第 2 步），`AGENTS.md:135` 的差异登记为已知文档滞后，08:19:08 行追加 R9 补记；② `GII-07` 补偿动作：网站侧先完成第 2 步前置，再 `libra revert -n` 只暂存、评审后按第 6 步签名提交并核验部署；网站走未变更文件路径时无网站补偿；幂等检查改为 `libra log --grep 'This reverts commit <SHA>'`；③ `GII-21` 按 G-03 重写（映射删去 AC 之外的 stderr 断言，前置命令标为执行条件）：AC1 改为 `ls-files -s a/b a/b-2` 输出逐字等于期望文本，原 AC5/AC6 改为合并后分别核对 `a/b/c/d` 与 `a/b-2`（合并须退出 0 作为执行条件），仍为 `8/8`；粒度规则补写「多路径断言各计一条、单一输出逐字比较计一条、前置命令为执行条件」；④ `GII-21` 撤销文档 `N/A`：`op` 与 `add`/`commit`/`reset`/`checkout`/`merge` 的 EN/zh/开发文档、`COMPATIBILITY.md` 对应行及 6 个网站页纳入同卡交付，GC-05、文档与网站验收命令、默认覆盖、追溯表与兼容收口同步；⑤ 网站第 2 步注明「不在 `cf` 即 STOP、不代为切换」比模板 ER-06a 更严；⑥ 全文复核复合谓词：粒度规则写明「退出码非判定对象时为执行条件」「两个可分别失败的状态事实拆为两条」，据此 `GII-03` 原 AC2 拆为 AC2/AC3（`5/8`）、`GII-16` 原 AC6 拆为 AC6/AC7（基准改为 AC8，`8/8`），`GII-05` AC1、`GII-12` AC4/AC5、`GII-17` AC3–AC5 与 `GII-01` AC5 映射改为单一比较 + 执行条件 | N/A | 文头、文档职责、修订历史（08:19:08 行补记）、与其它计划的关系、评审结论、全局工程约束、执行检查必备需求、实施顺序、依赖登记表、Phase 0、粒度规则、字段默认、审计表、GII-01、GII-03、GII-05、GII-07、GII-12、GII-16、GII-17、GII-21、追溯表、里程碑、兼容与文档收口、Review log |
| 2026-11-18 12:05:06 UTC | 双审 R10：Claude FAIL（1×P1、1×P2）、Codex FAIL（3×P1），受审 SHA-256 `d70cff65f7c6…` | ① `GII-07` 补偿动作改为两仓库统一流程：`libra revert -n` 只暂存 → `libra diff --staged` 交评审并记录暂存 blob OID → 核对暂存集合后 `libra commit -s -m` 写入逐字标记 `This reverts commit <完整 SHA>.` → 核验 `gpgsig`、`Signed-off-by`、说明含标记、提交文件集合与 `HEAD:<path>` blob OID 等于受审暂存 blob；幂等检查用 `libra log --grep ... --format=%H`；本仓库撤销仅在 `GII-08` PR 合并前执行，冲突即 STOP；② `GII-14` AC4 映射删去 AC 之外的 stderr 断言 | N/A | 文头、GII-07、GII-14、Review log |
| 2026-11-18 12:08:56 UTC | 双审 R11：Claude PASS（1×P2）、Codex FAIL（3×P1），受审 SHA-256 `7921b12f8c2c…` | `GII-07` 补偿动作重写：不再使用 `libra revert`（会以 `HEAD` 重建 index 并改写已跟踪文件、覆盖无关改动）；撤回时 `GII-07` 立即置 `blocked`、`Acceptance` 置空（已完成亦回退并登记）且承载例外对其失效（不随 `GII-08` 自动完成，只能重新交付并重跑全部 `Verification` 或登记 `DEFER-*`）；撤回动作改为按 ER-10 登记 `FIX-GII-07`（`docs` / `no-release`，承载点为下一张未完成的 `independent` 卡）以人工编辑删除 `0666` 示例并走常规门禁，网站部分仅在曾产生网站提交时按第 2–7 步交付（第 7 步判据反向：线上不再含 `M`）；幂等键改为 `FIX-GII-07` 卡 ID，中断后按记录从首个未完成步骤继续，提交后的核验、推送、远端核对与部署核验不得因已有提交而跳过；「实施顺序」承载例外同步注明失效条件 | N/A | 文头、实施顺序、GII-07、Review log |
| 2026-11-18 12:13:04 UTC | 双审 R12：Claude FAIL（1×P1、1×P2）、Codex FAIL（4×P1），受审 SHA-256 `426c3770e8a1…` | ① `GII-07` 补偿动作改为前滚：`FIX-GII-07 -> GII-07` 以 `FIX-GII-07` 达本地验收（含 ER-05 `PASS` 与独立本地提交）为满足条件，`GII-07` 随后重跑全部 `Verification` 回到 `locally-accepted`；承载点按触发时机为 `GII-08`（未进入 C 组）、下一张未进入 C 组的 `independent` 卡 X，或在已无 X 时由 `FIX-GII-07` 改为 `independent` 自行执行 C/D，承载卡只依赖两卡的本地条件，消除死锁并补足无承载点的情形；② 网站第 2 步⑤ 记录对账一致时的 `origin/cf` SHA，并新增「续作例外」：按已记录的写后 SHA-256、受审 SHA-256、本卡提交 SHA 与 `origin/cf` 记录识别「已写入未暂存 / 已暂存未提交 / 已提交未推送 / 已推送」四种本卡残留（含「已提交但 SHA 未记录」的崩溃窗口，按提交父节点、文件集合、受审 SHA-256 与提交说明识别）并从对应步骤续作，其余状态仍 STOP；`FIX-GII-07` 在已无承载卡时改登记为 `implementation` / `independent`；③ 第 6 步远端核对改为 `libra ls-remote origin refs/heads/cf` 恰一行且其第一列 hash 等于 `libra rev-parse HEAD`（`src/command/ls_remote.rs:403`）；④ `GII-02` AC3 映射把退出 128 标为执行条件、只判 stderr 非 JSON 信封，原保留的 P2 已修，不再有未具名接受的残余 P2 | N/A | 文头、执行检查必备需求（第 2、6 步）、实施顺序、GII-02、GII-07、Review log |
| 2026-11-18 12:16:29 UTC | 双审 R13：Claude FAIL（1×P1）、Codex FAIL（4×P1），受审 SHA-256 `bd8f9f8241c1…` | ① `GII-07` 补偿的网站部分改为按补偿后的目标内容决定：`FIX-GII-07` 一律对 `init.en.md` 执行第 2、3 步同步并按逐文件路径交付（有变化走变更路径，无变化核对已提交内容与线上并在不一致时走部署恢复）；② 删除场景以待删除旧句 `D` 代替 `M` / `W`，写前要求网站文件与线上均含 `D`、写后网站文件不含、推送后轮询至线上不含，未变更路径要求已提交内容与线上均不含；③ `GII-07` 的 B 组结构与链接门标为 ER-04 强制门、不计入 VER，`VER=6/20`（卡内与审计表同步）；④ `GII-07` 撤回登记 `DEFER-*` 时承载卡只需 `FIX-GII-07` 满足本地条件，且撤回后 `GII-08` 对 `GII-07` 的前驱条件改按补偿动作② 的本地条件判定（不退回「前驱须 `done`」），消除延后与撤回路径的阻塞；⑤ 原 `10:35:00` 行的时间列改为「时间不可考（最迟于 2026-11-18 10:11:04 UTC 已存在）」，不再以上界充当事件时间 | N/A | 文头、修订历史（原 10:35 行时间列）、审计表、GII-07、Review log |
| 2026-11-18 12:19:20 UTC | 双审 R14：Claude PASS、Codex FAIL（2×P1），受审 SHA-256 `8b906260c235…` | ① Phase 1、Phase 2 退出条件与里程碑 M2 排除正式登记 `DEFER-*` 的卡（撤回后延后的 `GII-07` 须其 `FIX-GII-07` 已完成），与「完成判据」的「所有非延后任务」一致；② `GII-07` 补偿删除场景改为：修正后的文档必须新增一句说明句作为 `M` / `W` 正向绑定部署，`D` 只作追加判据——写前要求网站文件含 `D`、线上 `D` 状态只记录不作前置，写后文件不含 `D`，推送后轮询至线上含 `M` 且不含 `D`；未变更路径要求已提交内容不含 `D` 并核对 `W` | N/A | 文头、Phase 1、Phase 2、里程碑、GII-07、Review log |
| 2026-11-18 12:35:16 UTC | 双审 R15：Claude PASS、Codex FAIL（3×P1），受审 SHA-256 `d7914c6f7d90…`；接续 Agent 修订 | ① `GII-07` 补偿动作改为模板 ER-10 前滚，删去自创的 `FIX-GII-07` 承载 (a)/(b)/(c)、撤回后 `DEFER-*` 与删除句 `D` 判据：补偿命令为两仓库新提交（网站按第 2–7 步），`GII-08` 开工前发现属本卡就地修复（`Acceptance` 置空、必要时按 ER-03 修订 AC13–AC17 后重跑全部 `Verification` 与 ER-05），开工后发现按 ER-10 新建 `FIX-*` 并经 ER-03 计划修订登记，本卡验收记录保持事实原状；「实施顺序」承载例外改为允许就地修正追加提交、`blocked` 时不得随 `GII-08` 完成；② 网站第 3、7 步不再写快照文件：新增第 7 步「页面检查」（`curl -fsSL` 输出只存 shell 变量、`curl` 非 0 即 STOP、`rg -F -q` 判定），第 3 步未变更文件直接读取工作区文件（状态为空即等于已提交内容），纯删除改动须新增说明句作为 `M`；③ 「完成判据」的 `Verification`、全量门、网站门与发布卡条目，以及成功定义、Phase 0/1/2 退出条件与 M1–M3 统一只覆盖非延后卡，「非目标与延后项」补正式延后登记规则（承接位置、issue 部分闭环）；网站门改为「在本卡内」完成（与 `GII-07` 网站在本卡交付一致）；④ `GII-16` AC4–AC7 改为比较 `object_index` 完整行列表与 marker 文件名及内容字节，恢复后逐个核对 marker 所记对象行并要求 marker 目录无文件；⑤ 六类清扫：新增 `DEP-GII-03`（同批计划相交写集含 `COMPATIBILITY.md` 的协调者串行窗口）并挂到全部 22 张卡、审计表、「与其它计划的关系」与风险登记；`GII-05` AC1 改为仅全局配置的别名（区分只读本地配置的错误实现）；`GII-09` AC2、`GII-20` AC1/AC4、`GII-22` AC1 删去映射未断言的错误码、与映射一致；`GII-12` AC6 映射要求执行前 `HEAD` ≠ `M`；`GII-14` AC2 措辞与 `ls-files -s` 全量比较一致；模板「Verification 判定口径」引用标明出处 | N/A | 文头、成功定义、与其它计划的关系、评审结论、执行检查必备需求（第 3、7 步）、实施顺序、依赖登记表、Phase 0/1/2、审计表、全部任务卡 `Dependencies` / `Granularity`、GII-05、GII-07、GII-09、GII-12、GII-14、GII-16、GII-20、GII-22、里程碑、风险登记、非目标与延后项、完成判据、Review log |
| 2026-11-18 12:42:11 UTC | 双审 R16：Claude FAIL（1×P1、1×P2）、Codex FAIL（3×P1），受审 SHA-256 `43b8843a866d…` | ① 网站第 3、7 步的判定句 `W` / `M` 另须不含会被 HTML 转义的 `'`、`"`、`&`、`<`、`>`（实测 `https://libra.tools/en/docs/commands/init` 以 `&#x27;`、`&amp;` 输出），措辞改为「页面 HTML 源逐字包含」；② 「非目标与延后项」的延后规则改为须经 ER-03 计划修订生效：逐一评估串行链后继、Phase 进入条件与承载关系，仅为写集串行的后继改依赖被延后卡的前驱，有真实功能前置的后继随之延后或保持 `blocked`，被延后的承载卡须为其 `no-release` 卡改指具名新承载点；「实施顺序」注明下游依赖只能按该规则改写；③ `GII-16` 故障点改为本卡在 `ObjectIndexTestFaults` 新增、位于事务内写入语句之后与提交之前的 `#[cfg(test)]` 字段并以触达计数证明（既有 `update_failure` 在写入前返回，`client_storage.rs:3961-3964`，不得替代）；夹具 marker 数等于测试下单事务批大小（`INDEX_REPAIR_BATCH_SIZE = 2`）；恢复改为重复回放至 `remaining=false`（至多 5 次），AC6、AC7 措辞同步；④ `GII-07` 补偿按「文档与网站验收命令」第 1–7 步验收仓库文档与网站（Claude P2）；⑤ 统一失败类 AC 口径（协调者 2026-11-18 转达的已验证做法）：粒度规则写明失败类 AC 只断言稳定错误码、不断言数值退出码，需断言失败文案时以非 0 退出为执行条件；据此 `GII-01` AC7–AC8、`GII-02` AC3–AC6、`GII-03` AC5、`GII-04` AC4、`GII-05` AC5–AC7、`GII-06` AC4、`GII-07` AC11、`GII-09` AC1–AC3、`GII-12` AC7–AC8、`GII-19` AC5、`GII-20` AC1/AC3/AC4、`GII-22` AC1–AC2 的 AC 与映射改为同一谓词，删去 `GII-05` AC5/AC7 映射中 AC 之外的断言（别名名、`pwned` 文件）；`GII-14` AC1 与 AC3 只断言 `LBR-CLI-003`，Git 文案改为独立的 AC7（`AC=7/8`，卡内与审计表同步）；`GII-06` AC3 映射补与「无模板」建库的顶层条目集合比较，与 AC 文本同一谓词 | N/A | 文头、执行检查必备需求（第 3、7 步）、实施顺序、任务卡粒度规则、审计表、GII-01、GII-02、GII-03、GII-04、GII-05、GII-06、GII-07、GII-09、GII-12、GII-14、GII-16、GII-19、GII-20、GII-22、非目标与延后项、Review log |
| 2026-11-18 12:57:57 UTC | 双审 R17：Claude PASS（2×P2）、Codex FAIL（4 分区 A/B/C/D，共 16×P1），受审 SHA-256 `e1401a6dda9b…` | ① 网站第 4 步登记 `pnpm typecheck` / `build` 重写的被忽略生成文件（`docs-manifest.ts`、`docs-search.ts`，`AUTO-GENERATED` 标注缺失即 STOP，前后 SHA-256 只记录，不得暂存）；② `GII-02` AC1/AC2 改用与 `--git-dir` 父目录不同的工作树 `<w>`（期望 `?? w.txt`）；`GII-03` 新增 AC6（`GIT_WORK_TREE`）、AC7（显式 `--work-tree` 优先），`7/8`；`GII-05` 删去由 `GII-19` 规则提供的原 AC4，改增 AC7「`!` 别名不被执行」（`pwned` 不存在），仍 `8/8`；`GII-01` 只支持 argv 首位单个 `-C`，原 AC3（重复 `-C`）移至 `GII-19` AC7（`7/8`），`GII-01` AC3 改为非首位 `-C` 以 `LBR-CLI-002` 拒绝的已文档化限制，ADR-GII-01 结果段、两卡说明与文档措辞同步；`GII-07` 复现脚本改判完整的 `Error-Code: LBR-CLI-002`；③ `GII-10` AC1/AC3 改为与 pinned Git `status -v` / `-vv` 同构格式的逐字节比较（覆盖 `-vv` 保留已暂存 diff），AC5 比较每次调用前后的完整 HEAD（符号引用 + OID），AC6 改为每次调用前后；`GII-11` AC1 比较与 pinned Git 同构的 `status --short` 行集合，AC3 改判 `d/f` 删除行；`GII-12` AC5 核对两端点行与全部 `^` 行；④ `GII-13` 映射要求外层与嵌套冲突区个数与 pinned Git 一致且 ≥ 1；`GII-15` 新增 AC6（调用不早于锁释放返回）并以 `ConfigDbFixture` + `#[serial(env)]` 隔离配置（写集补 `tests/SERIAL_REGISTRY.tsv`、`.config/nextest.toml`，`6/8`）；`GII-18` AC1–AC4、AC6 直接调用 `verify_object` 并断言 `CheckStatus`（期望 OID 按原实现计算），AC5 改为 `verify_object` 函数体结构守卫；`GII-21` AC8 改由 `scan_working_copy` 单元用例直接断言 `ScanResult.tracked` 不含 `d/f`（`VER=2/8`，追溯表同步）；`GII-14` 说明「index 零写入」改为「index 条目不变」 | N/A | 文头、ADR-GII-01、执行检查必备需求（第 4 步）、审计表、GII-01、GII-02、GII-03、GII-05、GII-07、GII-10、GII-11、GII-12、GII-13、GII-14、GII-15、GII-18、GII-19、GII-21、追溯表、Review log |
| 2026-11-18 13:09:28 UTC | 双审 R18：Claude FAIL（1×P1、1×P2）、Codex FAIL（4 分区，共 15×P1），受审 SHA-256 `d0c2a2cc08b3…` | ① 网站第 4 步改写的被忽略生成产物（`docs-manifest.ts`、`docs-search.ts`、`dist/`）写明归属与不保留策略，列入全部 19 张含网站文件卡的写集，并在 `DEP-GII-03` 中视为与所有同批计划的网站交付相交、只在协调者串行窗口内执行；Phase 2 目标改为消除 `ELOOP` 中止；② `GII-01` AC5、`GII-19` AC6 改为验证错误渲染路径（`src/main.rs:699-703` 以原始 argv 决定输出模式），`GII-05` AC3 改为别名展开后的 JSON 错误信封；`GII-07` 把三份文档的「含示例」与「结果一致」拆为 AC13–AC18，`COMPATIBILITY.md` 为 AC19、网站为 AC20（`20/20`、`VER=9/20`），行数不变归入 ER-06a 第 1 步；③ `GII-08` AC1 断言完整 Git 文案并以 `init -b main` 固定初始分支；`GII-09` AC8、`GII-20` AC8、`GII-22` AC5 覆盖无 schema 守卫预检路径（`count-objects`），`GII-22` AC6 覆盖 `blake3`；`GII-12` 按 G-09 拆出 `GII-23`（`checkout -b A...B` 单一起点：建分支、切换 HEAD、工作区与 index、两种失败及失败前后完整仓库快照，`7/8`），链改为 `GII-12 -> GII-23 -> GII-13`；④ `GII-15` AC6 改用通道 `try_recv` 在释放锁前确认调用未返回；`GII-16` AC3 改为以 debug 测试钩子证明 `add` 走批量回放（`enqueued=0 replay_tx=2`）；`GII-17` 新增 AC7 经共享入口 `deadline_provider_extraction_summary` 验证 Hermes 分派；`GII-18` 删去可被绕过的结构守卫（就地哈希的效果由基准判定），`6/8`、`VER=2/8`；`GII-21` AC8 改为 #1014 夹具上 `scan_working_copy` 须返回 `Full` 快照（先在未修复树上复现失败） | `GII-12` → `GII-12`, `GII-23` | 文头、使用规则、当前缺口、评审结论、GC-05、执行检查必备需求（第 4 步与适用卡）、实施顺序、依赖登记表、Phase 2、审计表、GII-01、GII-05、GII-07、GII-08、GII-09、GII-12、GII-13、GII-15、GII-16、GII-17、GII-18、GII-19、GII-20、GII-21、GII-22、GII-23、全部含网站文件卡的写集、追溯表、里程碑、风险登记、Review log |
| 2026-11-18 13:16:21 UTC | 双审 R19：Claude PASS（1×P2）、Codex FAIL（4 分区，共 16×P1），受审 SHA-256 `3e1b17b7edac…` | ① 粒度规则改为与模板一致：同一谓词在一组同类对象上的全称断言计一条（映射须覆盖全部成员），不同谓词或场景各计一条——据此 `GII-13`、`GII-16` 的逐文件 / 逐对象断言计数成立；错误码基线补 `LBR-REPO-003`、`LBR-IO-002`；网站第 4 步允许被忽略的生成文件不存在（记 `absent`、由本步生成，执行后须存在且带标注）；② `GII-03` AC3 改为前后完整 HEAD 快照；`GII-04` 新增 AC5（存储目录内 `--bare status` 以 `LBR-REPO-003` 失败，证明其它命令走裸上下文）与 AC6（前后 `config list` 相同，`--bare` 不持久化），写集补 `rev_parse.rs`、`util.rs`（`6/8`、`landing=4`）；`GII-05` AC2 一次覆盖单引号、反斜杠转义与其余参数拼接，AC3 断言 JSON 信封 `error_code`；`GII-06` 新增 AC5（超长路径告警，`5/8`）；`GII-19` AC2 同时覆盖无值、`=` 可选值与必选值标志；③ `GII-10` diff 期望改为 pinned Git 实测文本常量（不由 Libra 的 `diff` 生成）；`GII-12` AC5 改为保留重复的排序列表比较；④ `GII-14` 新增 AC8（`mv -f` 失败后 index 不变，`8/8`），说明不再承诺 `-f` 的文案；`GII-15` AC2 生效行标为执行条件；`GII-18` AC5 改用可解码而内容不符的对象并断言摘要比较分支的 `Hash mismatch: expected`；`GII-21` AC8 改为与期望快照（已跟踪 / 未跟踪映射与 `Full`）一次比较；`GII-23` 新增 AC8（右侧省略 `A...` 取 HEAD，`8/8`），左侧省略移入 Out of scope，`RS` 的工作区部分改为 `lstat` 取得的类型、权限、内容或链接目标 | N/A | 文头、事实基线、任务卡粒度规则、执行检查必备需求（第 4 步）、审计表、GII-03、GII-04、GII-05、GII-06、GII-10、GII-12、GII-14、GII-15、GII-18、GII-19、GII-21、GII-23、追溯表、Review log |
| 2026-11-18 13:20:43 UTC | 双审 R20：Claude FAIL（1×P1）、Codex FAIL（4 分区，共 11×P1），受审 SHA-256 `98d0b7b12ab2…` | ① 网站步骤：逐文件 `status --short` 加 `--no-branch`（`status.branch=true` 会为短格式输出分支头，`src/command/status/input.rs:48-55,821`），新增「退出码捕获」通则，「页面检查」改为 `if …; then rc=0; else rc=$?; fi` 形式；② `GII-02` AC3 改为正向断言人读标记 `Error-Code: LBR-REPO-001`；`GII-04` AC6 改为直接比较 `config` / `config_kv` 两表全部行；`GII-07` AC11 以 `mode0400_rc` 非 0 为执行条件；`GII-01`/`GII-02`/`GII-03` 的逐字 `status --short` 输出加 `--no-branch`；③ `GII-12` AC5 与 `GII-23` 的 criss-cross 夹具改为按构造已知两个 merge-base（期望不依赖 Libra 的 `merge-base`）；`GII-11` 删去无法观测的「不经链接访问」承诺，AC2 改述为可观察的删除报告；`GII-09` 更正重初始化与 Git 源转换两条既有校验的归属，并写明拆出 `GII-22` 与 `GII-20`；④ `GII-21` 说明补最终分量符号链接须以不跟随链接的元数据与目标字节哈希（`snapshot.rs:703-704` 的 `fs::metadata` 会跟随），AC8 改为在关闭 stat 短路的扫描器上比较整个 `ScanResult`（含 `bytes` 与 `reused_index_oid`）；`GII-17` AC1/AC2 改为对非测试部分的结构守卫（Hermes 专用实现引用与字面量 `"hermes"`），可被 `.eq("hermes")` 等写法绕过的 `rg` 守卫作废（Claude P1） | N/A | 文头、执行检查必备需求（文档与网站验收命令）、GII-01、GII-02、GII-03、GII-04、GII-07、GII-09、GII-11、GII-12、GII-17、GII-21、GII-23、Review log |
| 2026-11-18 13:26:05 UTC | 双审 R21：Claude PASS、Codex FAIL（4 分区，共 7×P1），受审 SHA-256 `38f5b313de11…` | ① 「页面检查」改用 here-string 传入 `rg`（`pipefail` 下管道上游会因 `rg -q` 提前退出得 `SIGPIPE` 141）；粒度规则与「验证执行协议」第 4 条写明「被测命令以非 0 退出」是每条失败类 AC 的执行条件（不固定数值）；② `GII-06` AC1/AC3 改为比较递归相对路径集合（空模板与默认模板的差异经已安装 0.30.207 实测），新增 AC6（两仓库均可打开，`6/8`）；`GII-11` AC1 两侧加 `--no-branch`；③ `GII-16` 测试钩子限定为 `add` 暂存批的计数（不含 operation snapshot 写入）；`GII-15` AC6 增加 `STORAGE_TYPE_DB_READ_SUCCESSES` 计数，要求释放前为 0、返回后 ≥ 1；`GII-17` AC1/AC2 改由新增的「GII-17 结构守卫脚本」判定（去注释后按 Rust 规则解码字面量，并与开工基线比较函数集合、拒绝 Hermes 模块引用），在当前源码、修复后源码及转义字面量 / 改名函数两种绕过写法上核对了退出码 | N/A | 文头、任务卡粒度规则、执行检查必备需求（验证执行协议第 4 条、页面检查）、审计表、GII-06、GII-11、GII-15、GII-16、GII-17、Review log |
| 2026-11-18 13:30:00 UTC | 双审 R22：Claude PASS（1×P2）、Codex FAIL（4 分区：C 区 PASS，A/B/D 共 5×P1），受审 SHA-256 `ec34f6da1317…` | ① 「完成判据」最终树全量门改为记录被测提交 `T`，其后只含本计划文件与 `plan-status.md` 的证据提交不触发重跑，消除自引用；② `GII-02` 新增 AC7（空 `--work-tree=`）、AC8（两个带值标志在 argv 末尾缺值），`8/8`；`GII-06` 把两仓库可用拆为 AC6、AC7（`7/8`），映射按 AC 顺序排列（Claude P2）；③ `GII-15` AC6 改为以 `s3` + 空 bucket 的可区分仓库配置证明使用了释放后的读取结果（新增用例 `storage_type_read_after_lock_uses_repository_config`，删去只计成功次数的 `STORAGE_TYPE_DB_READ_SUCCESSES`），用例与 `Verification` 同时清除 `LIBRA_STORAGE_BUCKET`；`GII-17` 守卫改为单遍词法扫描（字符串先于注释识别），在 URL 字面量、原始字符串、转义字面量、改名函数与仅注释提及等情形上核对退出码 | N/A | 完成判据、文头、审计表、GII-02、GII-06、GII-15、GII-17、追溯表、Review log |
| 2026-11-18 13:35:03 UTC | 双审 R23：Claude PASS、Codex FAIL（4 分区：C 区 PASS，A/B/D 共 3×P1），受审 SHA-256 `a2053d76b627…` | ① 「完成判据」最终全量门要求运行前后 `libra status --porcelain --untracked-files=all` 除本计划与 `plan-status.md` 外为空（否则该次运行无效），或在检出 `T` 的独立副本中运行；② `GII-02` 按 G-09 拆出 `GII-24`：`GII-02` 只交付等号形式与空值拒绝（空格分隔形式按现状以 `LBR-CLI-002` 拒绝并由 AC2 验证，`6/8`），`GII-24` 交付空格分隔形式、以 `-` 开头的取值（`--git-dir --json` 的错误渲染）与两个标志各自的末尾缺值（`4/8`）；链改为 `GII-19 -> GII-02 -> GII-24 -> GII-03`，ADR-GII-01 结果段、文头、使用规则、当前缺口、评审结论、与其它计划的关系、GC-05 与网站门适用卡、实施顺序、并发声明、Phase 0、M1、审计表、追溯表与风险登记同步为 24 张卡；③ `GII-21` AC7 改为悬空链接夹具上的完整 `ScanResult` 比较（新增单元用例 `scan_completes_full_on_dangling_symlink`，`VER=3/8`），文档影响改述为「互指链接的致命中止改为完成、悬空链接的快照不再降级为不完整」 | `GII-02` → `GII-02`, `GII-24` | 文头、使用规则、当前缺口、与其它计划的关系、评审结论、ADR-GII-01、GC-05、执行检查必备需求（网站门适用卡）、实施顺序、并发声明、Phase 0/2、审计表、GII-02、GII-03、GII-21、GII-24、追溯表、里程碑、风险登记、完成判据、Review log |
| 2026-11-18 13:38:12 UTC | 双审 R24：Claude PASS、Codex FAIL（4 分区，共 5×P1），受审 SHA-256 `d07ed5f26603…` | ① 「完成判据」最终全量门在运行前后同时核对 `HEAD`、`HEAD^{{tree}}` 等于 `T` 及其树与工作区状态，独立副本同样执行三项检查；② `GII-02` 新增 AC7（空格分隔 `--work-tree` 按已文档化限制拒绝，`7/8`）；`GII-24` 新增 AC5（`--work-tree --json` 的取值按路径处理、错误以人读格式输出，`5/8`）；`GII-10` 新增 AC7（每次 `-v` / `-vv` / `--verbose` 调用前后工作区快照相同，`7/8`）；③ `GII-18` 说明改为调用已就地实现的 `utils::object::git_object_hash`，新增 AC7 与函数体结构守卫（须以 `&data` 调用 `git_object_hash` 且不再调用 `object_format::digest`；在当前源码上核对退出 1），`7/8`、`VER=3/8` | N/A | 完成判据、文头、审计表、GII-02、GII-10、GII-18、GII-24、Review log |
| 2026-11-18 13:40:40 UTC | 双审 R25：Claude PASS、Codex PASS（4 分区全部 PASS），受审 SHA-256 `ccdb789c8848…` | 修正 Codex C 区 P2：`GII-22` AC1 的夹具改为显式 `libra init --object-format=sha1`，不再依赖默认对象格式（`GIT_DEFAULT_HASH`、`init.defaultObjectFormat` 会改变默认值）；内容因此变化，按收尾规则对新字节进行仅差异确认轮 R26 | N/A | 文头、GII-22、Review log |
| 2026-11-18 13:41:58 UTC | 双审 R26（仅差异确认）：Claude PASS、Codex PASS，受审 SHA-256 `973afedc5a696064bd282f88b9041585d2342e7f959e51a55bb7fa1470085f44` | 记账：R25 由 Claude 与 Codex（4 分区）对 `ccdb789c8848…` 全文 PASS，R26 由两名评审确认其后唯一内容变更（`GII-22` AC1 夹具）无新缺陷，二者对同一受审字节 `973afedc5a69…` 均 `VERDICT: PASS`，计划可提交；本行、对应 Review log 两行与文头状态行为 PASS 后的记账修改 | N/A | 文头、Review log |

## 已决议设计决策

### ADR-GII-01: 全局 `-C` / `--git-dir` / `--work-tree` / `GIT_DIR` / `--bare` / `alias.*` 预处理顺序

- **Status:** Accepted
- **Context:** #686、#801、#944、#945、#987 涉及 `src/cli.rs` 与 `src/main.rs` 的启动预处理与仓库发现顺序。
- **Decision:** 在 `main.rs`/`cli.rs` 启动管线按 Git 顺序执行：① 在遇到 `--` 或首个非全局子命令 token 前扫描全局参数：共享 `src/cli.rs` 权威全局 flag 表识别全部**既有**无值标志（`--machine`、`--no-pager`、`--no-color`、`--quiet`/`-q`、`--exit-code-on-warning`、`--sync-data`、`--offline`、`--literal-pathspecs`、`--no-literal-pathspecs`）与带值/可选值标志（`--json[=<fmt>]`/`-J`、`--color[=<when>]`）与必选值标志（`--progress <MODE>`/`--progress=<MODE>`、`--max-connections <N>`/`--max-connections=<N>`），以及**新增**全局标志（`-C <path>` / `-C<path>`、`--git-dir=<p>` / `--git-dir <p>`、`--work-tree=<p>` / `--work-tree <p>`、`--bare`；三个带值标志缺值返回 `LBR-CLI-002`；`-C ''` 按 Git 文档为不切换目录的空操作；`--git-dir=` / `--work-tree=` 空值按 Libra fail-closed 取值返回 `LBR-CLI-002`），跳过既有全局标志的值 token 而不提前终止或误判为子命令，使 `libra --json -C <dir> status` / `libra --max-connections 8 -C <dir> status` 等任意前后交错组合均按出现顺序对 `-C` 执行 `chdir`；② 解析 `--git-dir`/`--work-tree`/`--bare` 与环境变量 `GIT_DIR`/`GIT_WORK_TREE`（CLI 优先于环境变量）；③ 若子命令 token 既非内置命令也非 clap 内置别名（如 `st`，`src/cli.rs:355`），读取 `alias.<name>` 并按 POSIX 引号/转义规则分词展开（未闭合引号报 `LBR-CLI-002`，`!` shell alias 明确报错不支持，最大展开深度 16 防环），再交由 `clap` 解析；④ `OutputConfig::resolve_from_argv`（`src/utils/output.rs:250-327`）作用于**预处理后的 argv**，并跳过 `-C`/`--git-dir`/`--work-tree` 的值及 `--` 之后参数，避免路径或透传参数被误判为输出标志。
- **Alternatives considered:** 在每个子命令内各自读取 `GIT_DIR` / `-C`（违反 GC-02，易遗漏命令，拒绝）。
- **Consequences:** `GII-01`、`GII-19`、`GII-02`、`GII-24`、`GII-03`..`GII-05` 串行修改 `src/cli.rs`/`src/main.rs` 并均受 `DEP-GII-01` 门控（`GII-01` 交付 ① 中 argv 首位单个 `-C` 的识别与 ④ 中 `-C` 路径值的剔除；`GII-19` 交付 ① 中重复 `-C`、既有全局标志值的跳过与 ④ 中在 `--` 处停止；`GII-02` 交付等号形式 `--git-dir=` / `--work-tree=` 的识别、剔除与空值拒绝，`GII-24` 交付空格分隔形式取值（含以 `-` 开头的取值）的剔除与末尾缺值拒绝，`GII-05` 交付别名展开后的解析）。
- **Revisit when:** 若后续引入内置 shell alias 或 daemon 模式再重审进程 `cwd` 切换。

### ADR-GII-02: Hermes extraction 严格回归 `ADR-ACF-10` 冻结基线（#656）

- **Status:** Accepted
- **Context:** #656 报告上游 `760a5529` 新增 `AgentKind::Hermes` 分支后，`src/internal/ai/capture/extraction.rs` 的非测试 `AgentKind::` 路径升至 16，超出 `tests/compat/agent_architecture_guard.rs:5846-5859` 冻结上限 `15`。2026-11-18 核对：当前源码改以 `agent_kind.as_db_str() == "hermes"` 字符串分派（`extraction.rs:421-422`）并在该文件内保留 `hermes_bounded_extraction`（`457-473`），计数为 14，但 provider 专用实现仍留在共享抽取模块。
- **Decision:** 将 Hermes 有界抽取实现下沉至 `src/internal/ai/observed_agents/extract.rs`（与 `extract_pi_bounded` 同层），`extraction.rs` 不保留 Hermes 专用实现与字符串字面量分派，非测试 `AgentKind::` 计数保持 ≤ 15；禁止调高 `agent_architecture_guard.rs` 中的冻结数字。
- **Alternatives considered:** 修改 `agent_architecture_guard.rs` 提高上限（违反 `ADR-ACF-10`，拒绝）。
- **Consequences:** `compat_agent_architecture_guard` 直接作为 `GII-17` 验收门。
- **Revisit when:** 仅当修订 `ADR-ACF-10` 时才可调整。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 的 `GC-01`..`GC-13`。特别强调：
- **GC-05 / ER-06a 文档同步:** 改变公开命令/全局参数行为的卡（`GII-01`..`GII-14`、`GII-19`..`GII-24`）同卡更新对应 `docs/commands/<cmd|README>.md`、`docs/commands/zh-CN/<cmd|README>.md`、`docs/development/commands/<cmd>.md`（全局标志无对应开发文档章节时记 `N/A`）、`COMPATIBILITY.md`，并按 `AGENTS.md:135` 的网站交付流程（路径以「文档职责」为准：`/Volumes/Data/gitmono/libra-backend`，用户 2026-11-18 11:55:09 UTC 确认；`cf` 分支，保留无关改动、不建新分支/PR/worktree）同卡交付：① 子命令页运行 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only <cmd> --libra-root /Volumes/Data/gitmono/libra`；② 全局 CLI 选项（`GII-01`..`GII-05`、`GII-19`、`GII-24`）同步更新 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`）；③ 运行网站文档检查与前端验证（`gen:docs` / `typecheck` / `build`）；变更的网站文件取得 Claude Code `PASS`、用 `libra commit` 提交并推送 `cf`、逐文件核验 Cloudflare 部署完成，未变更的网站文件按「文档与网站验收命令」第 3 步的未变更文件路径（线上未含本卡行为时走部署恢复路径）记录证据；若 `/Volumes/Data/gitmono/libra-backend` 不可用，fail-closed 停止网站交付。逐步命令与判据见「文档与网站验收命令」。纯内部修复卡（`GII-15`、`GII-16`、`GII-17`、`GII-18`）明确记录 `N/A` 理由。
- **测试串行登记（`compat_serial_registry`）:** 任一卡新增带 `#[serial(<key>)]` 的用例时，须在同卡于 `tests/SERIAL_REGISTRY.tsv` 登记并运行 `sh tests/NEXTEST_GROUPS.sh` 再生成 `.config/nextest.toml`；此时这两个文件并入该卡 `Implementation write set`（全计划单链串行，不产生并发写集冲突），且按 ER-13 `T-4` 跑全量。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 的 `ER-01`..`ER-14`：
- **ER-04 / ER-08 / ER-13 / ER-14:** 每张 `implementation` 卡均为 `independent` 发布切片，完成后按 ER-08 执行 `patch + 1`、同步 `compat_version_surface_sync` 权威三处（`Cargo.toml`、`install.sh`、`install.ps1`）、工具链刷新 `Cargo.lock`；因每卡均修改 `install.sh`/`install.ps1`（`T-1`），C 组第 ④ 步除运行本卡 A 组 focused `nextest` 外，必须跑双环境全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`、`fmt`、`clippy`、`cargo build --release`，随后签名提交推送 PR；因本工作区为 `.libra` 仓库（无 `.git`），squash merge 生成新提交后，发布前必须执行 `libra fetch` 同步远端 `main`、核验该 squash-merge 提交 SHA 的 PGP 签名与 `Signed-off-by` DCO trailer、比较树对象 ID：`libra rev-parse '<reviewed-head>^{tree}'` 与 `libra rev-parse '<squash-sha>^{tree}'` 必须逐字相同（不得以位置参数形式的 `libra diff` 代替；如需差异明细用 `libra diff --old <reviewed-head> --new <squash-sha> --exit-code -s`，退出 0 才算无差异），且 `base.yml`/`codeql.yml` 在该精确 SHA 全绿；**止步条件：** 任一核验失败立即停止，严禁执行 `gh release create "v${VERSION}"`。
- **ER-04 D 组远端后置门（每张 `independent` 卡默认适用）:**
  - `base.yml`（PR / `push` `main`）：`format`、`clippy`、`redundancy`、`test` 全部 green（2026-11-18 核对该 workflow 仅此 4 个 job；被 `cancel-superseded-main.yml` 取消的 run 不计 PASS，须取同 SHA 的后续成功 run）。
  - `codeql.yml`（PR / `push` `main`）：CodeQL 扫描 green。
  - `release.yml`（`push` `v*` tag，由 `gh release create "v${VERSION}"` 触发）：`build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` 全部 green；并核验 `download.libra.tools` / stable manifest（`HEAD` 200、range `GET` 206）。

- **验证执行协议（对全计划每条 `Verification` 条目强制）：**
  1. **分类：** 每条 `Verification` 标明三类之一——「卡特有用例」（`(new)` 或既有具名用例，只用精确过滤 `-E 'test(=<完整测试名>)'`，多条用 `or` 并列精确名）、「共享回归组」（锚定模块的正则过滤 `-E 'test(/^<模块路径>::…/)'` 或整个 compat target）、「非 nextest 门」（基准、`rg` 零命中守卫等，按条目自带判据执行）。测试名以「事实基线」测试行的命名形式为准（集成测试 `command::<module>_test::<fn>`，单元测试为 crate 内完整模块路径）。
  2. **列举：** 运行任一 nextest 条目前，先以相同 target 与过滤执行 `cargo nextest list`（如 `cargo nextest list --test command_test -E '<同一过滤>'`）并记录列出的测试名：卡特有用例的列出集合必须与条目写明的完整测试名集合逐字相等（单名即恰好 1 条）；共享回归组必须 ≥ 1 条并记录条数（零匹配时 nextest 以 `error: no tests to run` 失败）。不满足即 STOP。
  3. **运行与判据：** 记录退出码与 nextest 汇总行 `N tests run: P passed, F failed, S skipped`（`N` 为 1 时写作 `1 test run`；某项为 0 时 nextest 可能省略该段，按 0 记）：要求退出码 = 0、`F` = 0、`N` = `P` = 第 2 步列出条数。`S` 是被过滤排除（及未选中的 `#[ignore]`）的测试数，只记录、不作判据。出现 `FLAKY` 须写明，`--retries` 不得掩盖最终失败。卡特有用例必须是 L1 确定性用例（不依赖 L2/L3 环境门，不得以打印 `skipped (...)` 提前返回）。
  3a. **压力门（`--stress-count <K>`）：** 第 2 步的列出集合仍须等于条目写明的名称集合（`k` 条）；判据改为退出码 = 0、汇总行为 `K/K stress run iterations: K passed`，且每个 `iteration i/K` 行的运行计数均为 `k tests run: k passed`（其后的 `skipped` 计数只记录）；不适用第 3 步的 `N = P = 列出条数`。
  4. **AC 绑定：** 每条卡特有用例在所在卡的「AC → 断言映射」中写明覆盖的 AC 编号、场景、期望退出码、stdout/stderr 关键文本与前后状态；只写测试名不构成 AC 证据。映射中只列判定对应 AC 所需的检查，不列 AC 之外的独立断言（凡可独立失败的行为断言都必须是一条计数的 AC）；失败类 AC 的用例另须断言被测命令以非 0 退出（执行条件，见「任务卡粒度规则」）。
  5. **共享回归组** 属 ER-04 A 组 focused 门，只证明既有行为未回归，不计为卡特有 AC 证据，也不计入 G-03 的 `VER` 分子（条目标注「不计入 G-03 VER」）；判据同第 3 步。
  6. **记录：** 命令、列出条数、汇总行、退出码与 UTC 时间写入本计划对应卡的 `Verification` 条目（v2.12，不新建证据文件）。

- **文档与网站验收命令（`GII-01`..`GII-14`、`GII-19`..`GII-24` 的 ER-06/ER-06a 同卡门；`Docs and compatibility impact` 为 `N/A` 的卡不适用）：**
  - **退出码捕获（本节通用）：** 本节所有按退出码判定的命令（`test`、`rg`、`curl`、`libra merge-base --is-ancestor` 等）一律以 `if <命令>; then rc=0; else rc=$?; fi` 的条件形式捕获退出码（模板「Verification 判定口径」），在 `set -e` / `errexit` 下也不会因预期的非 0 退出（如 `rg` 的「不含」= 1）而提前中止；随后按各步写明的取值判定。
  1. **仓库文档逐文件验收：** 对 `Docs and compatibility impact` 列出的每个仓库内文件，记录修改前后 `shasum -a 256 <file>`；以 `rg -n -F '<本卡 flag 或行为关键词>' <file>` 定位（锚点定位用）并人工确认对应章节已描述本卡行为（EN 与 zh-CN 均需覆盖）；`COMPATIBILITY.md` 只原地改写既有行，`wc -l COMPATIBILITY.md` 前后相等。卡内写明「核对无需修改」的文件记录其 SHA-256 与核对结论。缺失任一文件或内容不符即 STOP。
  2. **网站前置（所有分支，第 3 步写入前完成）：** ① `cd /Volumes/Data/gitmono/libra-backend && test -d .libra && test ! -e .git`（退出 0 即仅含 Libra 元数据）；② `libra status --short --branch` 首行以 `## cf...origin/cf` 开头（按 `AGENTS.md:135` 直接在 `cf` 上工作：不在 `cf` 时 STOP 并报告，不代为 `libra switch`，比模板 ER-06a 的切换规则更严）；记录全部既有无关改动（路径与 SHA-256）并保持不动；③ `libra status --short --no-branch -- <本卡网站文件>` 输出为空（目标文件无他人未提交改动）；④ 暂存区为空：`libra status --porcelain` 输出中不存在首列（暂存区状态）为非空格且非 `?` 的行（Libra 提交整个暂存区，`src/command/commit.rs:253-255`，他人已暂存的改动会随本卡提交被推送）；⑤ 远端对账：`libra fetch origin` 退出 0 后比较 `libra rev-parse HEAD` 与 `libra rev-parse origin/cf`——相等即通过并在本卡记录该 SHA（供第 3 步无变更证据与「续作例外」使用）；不等且 `libra merge-base --is-ancestor HEAD origin/cf` 退出 0（本地仅落后）时执行 `libra pull --ff-only origin cf`（须退出 0）后从 ② 重新核验；不等且该命令退出 1（本地有未推送提交或已分叉）即 STOP 并报告归属，不得把他人提交随本卡推送。任一条不满足或任一命令出现非预期退出码即 STOP 并报告。
     - **续作例外（同一卡在第 3–7 步中断后重入）：** 重入时先执行 ①②，再以本卡已记录的证据识别本卡自己的残留，只放行能逐项对上的下列状态，其余一律按 ③–⑤ STOP：(i) **已写入未暂存**——各目标文件当前 SHA-256 等于第 3 步记录的写后值，暂存区为空，`libra rev-parse HEAD` 等于第 2 步记录的 `origin/cf` SHA：从第 4 步继续；(ii) **已暂存未提交**——`libra status --porcelain` 的暂存路径集合恰为本卡变更文件、这些行的第二列为空格（无未暂存改动），各文件 SHA-256 等于第 5 步受审值，`HEAD` 同 (i)：从第 6 步的提交继续；(iii) **已提交未推送**——`libra rev-parse HEAD` 等于第 6 步记录的本卡提交 SHA，`libra rev-parse 'HEAD^'` 等于第 2 步记录的 `origin/cf` SHA，且 `libra fetch origin` 后 `libra rev-parse origin/cf` 仍等于该记录值：从第 6 步提交后的核验继续并推送；若崩溃发生在提交之后、记录提交 SHA 之前，则仅当 `libra rev-parse 'HEAD^'` 等于记录的 `origin/cf` SHA、`libra show --name-only --format=format: HEAD` 恰为本卡变更文件、各文件 `libra show HEAD:<path> | shasum -a 256` 等于第 5 步受审值且提交说明为本卡第 6 步的说明时，才把该 `HEAD` 记为本卡提交并按 (iii) 继续；(iv) **已推送**——`libra ls-remote origin refs/heads/cf` 输出恰一行，其第一列 hash 等于记录的本卡提交 SHA：从第 7 步继续。无法归入上述任一状态或任一核对不符即 STOP 并报告。
  3. **网站同步与逐文件路径判定：** 子命令页先 `python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --check --only <cmd>...` 查看漂移（退出 1 = 有漂移，退出 0 = 无漂移，其它退出码即 STOP）；全局 CLI 选项核对 `apps/tanstack-app/content/docs/commands/index.en.mdx` 的 `## Global Flags` 是否已描述本卡行为。对将被写入的每个网站文件，写入前先按第 7 步为该文件选定 `M` 并确认零命中，再以 `--write --only <cmd>...` 写入（全局标志页手工更新），之后再次 `--check --only <cmd>...` 须退出 0。记录每个网站文件修改前后 SHA-256。随后**逐文件**以 `libra status --short --no-branch -- <该网站文件>` 判定其路径（同一张卡的不同网站文件可分属两种路径；卡级结果要求每个网站文件都通过其所属路径）：
     - **变更文件**（输出非空）：全部变更文件一并执行第 4–7 步，第 7 步逐文件核验各自的 `M`。
     - **未变更文件**（输出为空）：该输出为空且第 2 步④ 已确认暂存区为空，故工作区中的该文件内容即其已提交内容（直接读取，不另存副本）。人工确认其已描述本卡行为（子命令页应与第 1 步已验收的仓库文档同步文本一致；未描述即 STOP 并核查网站与仓库文档为何不一致），从中摘取一句描述本卡行为的纯文本句 `W`（不含反引号、链接、强调等 Markdown 标记，也不含会被 HTML 转义的 `'`、`"`、`&`、`<`、`>`（线上页面把它们输出为 `&#x27;`、`&amp;` 等实体），确保页面 HTML 源逐字包含），`rg -F -q -- '<W>' <网站文件>` 须退出 0（退出 1 即 STOP，>1 判执行失败）；再对该文件对应的线上页面执行第 7 步的「页面检查」（判定句为 `W`）：含 `W` → 记录第 2 步各项结果、`W`、该文件 SHA-256、`HEAD` 与 `origin/cf` 的 SHA 及线上命中时间作为该文件的「网站未变更」证据；不含 `W` → 该文件进入**部署恢复路径**：不为其新建提交，按 ER-10 有界轮询（≤ 5 次、总计 ≤ 30 分钟）执行「页面检查」直到含 `W`，记录命中时间与第 2 步记录的 `origin/cf` SHA；超预算仍未出现即本卡置 `blocked` 并报告（网站部署管线问题，由网站维护者处理），不得标 `complete`。
     - **无变更分支：** 全部网站文件均为未变更文件时，仍执行第 4 步，跳过第 5–7 步。
  4. **前端验证：** 在 `/Volumes/Data/gitmono/libra-backend` 运行 `pnpm typecheck`（含 `gen:docs`）与 `pnpm build`，均退出 0（无变更分支亦执行，以证明现状可构建）。`pnpm typecheck` 经 `gen:docs`（`apps/tanstack-app/package.json:12-13`）无条件重写被忽略的生成文件 `apps/tanstack-app/src/generated/docs-manifest.ts` 与 `docs-search.ts`（`scripts/gen-docs-manifest.ts:87,91,524-525,550`），`pnpm build`（`vite build`）重写被忽略的 `apps/tanstack-app/dist/`（`.gitignore:6,87-88`）。**归属与保留策略：** 这三处是网站仓库约定只由生成器 / 构建写入的派生产物（生成文件首行由生成器写入 `AUTO-GENERATED by scripts/gen-docs-manifest.ts — do not edit`，`scripts/gen-docs-manifest.ts:474,539`），按 `AGENTS.md:135` 的交付流程每次网站交付都必须重生成，旧内容不属于任何人的工作成果、不予保留；它们已列入每张含网站文件的卡的 `Implementation write set`，并按 `DEP-GII-03` 视为与所有同批计划的网站交付相交：第 4 步只在持有协调者对 `/Volumes/Data/gitmono/libra-backend` 网站交付的串行窗口时执行，从而排除他人在途使用。执行前逐个检查两个生成文件：不存在时记录 `absent`（被忽略文件在新检出或清理后本就不存在，由本步首次生成）；存在时首行不是该标注即 STOP 并报告（有人违反约定手工编辑），是则记录其 SHA-256。执行后两者都须存在且首行为该标注（否则判第 4 步失败），并记录其 SHA-256（前后哈希只记录、不作判据）；不得手工编辑、暂存或提交这三处（第 6 步的暂存与提交文件集合核对保证不会随本卡提交）。
  5. **独立评审：** 对本卡网站改动（`libra diff -- <网站文件>`）取得独立 Claude Code 字面 `VERDICT: PASS`，记录受审文件 SHA-256；评审后任一受审文件 SHA-256 变化即须重审。
  6. **提交与推送：** `libra add <本卡变更网站文件精确路径>` 后，`libra status --porcelain` 中首列非空格且非 `?` 的路径集合须恰为这些文件（否则 STOP）；`libra commit -s -m "docs(<cmd>): ..."` 后，`libra show --name-only --format=format: HEAD` 列出的非空行集合须恰为这些文件（否则 STOP，不得推送）；按 ER-07 核验同一提交的 `gpgsig` 与 `Signed-off-by`；记录 `libra rev-parse HEAD` 与 `libra rev-parse 'HEAD^{tree}'`，并以 `libra show HEAD:<path> | shasum -a 256` 确认提交内容等于第 5 步受审 SHA-256；`libra push origin cf` 后 `libra ls-remote origin refs/heads/cf` 须恰输出一行 `<hash>\trefs/heads/cf`（格式见 `src/command/ls_remote.rs:403`），其 `<hash>` 等于 `libra rev-parse HEAD`；推送被拒（远端已前移）即 STOP 并报告，不得强推。任一不符即 STOP。
  7. **部署核验（有变更分支，绑定本次改动）：** **「页面检查」**是本计划核对线上页面的唯一方法，页面内容只保存在 shell 变量中，不写任何文件（v2.12）：先 `if page="$(curl -fsSL '<页面 URL>')"; then rc=0; else rc=$?; fi`，`rc` 非 0 即 STOP（不得视为「不含」；`-f` 使 HTTP ≥ 400 以非 0 退出）；再 `if rg -F -q -- '<判定句>' <<<"$page"; then hit=0; else hit=$?; fi`（以 here-string 直接传入，不经管道，`pipefail` 下也不会因 `rg -q` 提前退出触发上游 `SIGPIPE`），`hit` 为 0 = 含、1 = 不含、>1 判执行失败；每次检查的 `rc`、`rg` 退出码与 UTC 时间写入本卡记录。页面 URL 为 `https://libra.tools/en/docs/commands` 或 `https://libra.tools/en/docs/commands/<cmd>`，以网站路由为准。在第 3 步写入前为每个将要修改的网站文件选定本卡新增的一句描述本卡行为的唯一纯文本句 `M`（不含反引号、链接、强调等 Markdown 标记，也不含会被 HTML 转义的 `'`、`"`、`&`、`<`、`>`（线上页面把它们输出为 `&#x27;`、`&amp;` 等实体），确保页面 HTML 源逐字包含；该文件的改动只删除文本时，须在同一改动中新增一句说明该删除的纯文本句作为 `M`），并按模板「Verification 判定口径」的退出码模板确认零命中：`rg -F -q -- '<M>' <网站文件>` 退出 1（>1 判执行失败），且对应页面的「页面检查」结果为不含；推送后按 ER-10 有界轮询（≤ 5 次、总计 ≤ 30 分钟）执行「页面检查」，直到含 `M`，记录命中时间与第 6 步的 `cf` 提交 SHA。若 `M` 在推送前已出现或超预算仍未出现，本卡置 `blocked`，不得标 `complete`。未变更文件（含无变更分支与部署恢复路径）按第 3 步执行，不选 `M`、不提交。

## 实施顺序

- 全计划严格串行链（消除 `COMPATIBILITY.md`、`src/internal/config.rs`、`src/cli.rs`、`src/command/init.rs`、`src/internal/operation/snapshot.rs`、`src/utils/client_storage.rs` 任何 I–I 并发重叠，G-10）：
  - Phase 0（全局 CLI）: `DEP-GII-01 -> GII-01 -> GII-19 -> GII-02 -> GII-24 -> GII-03 -> GII-04 -> GII-05`
  - Phase 1（`init`）: `GII-05 -> GII-06 -> GII-07 -> GII-08 -> GII-09 -> GII-22 -> GII-20`
  - Phase 2（核心与基础设施）: `GII-20 -> GII-10 -> GII-11 -> GII-21 -> GII-12 -> GII-23 -> GII-13 -> GII-14 -> GII-15 -> GII-16 -> GII-17 -> GII-18`
- 链上前驱默认以 `Lifecycle=done` 且 `Acceptance=complete` 为满足条件，唯一例外如下；某卡被正式延后时，其下游依赖只能按「非目标与延后项」的延后规则经 ER-03 计划修订改写。
- **`GII-07 -> GII-08` 承载例外：** `GII-07` 为 `audit` / `no-release` 卡，按 ER-04「`no-release` 卡」条继承 `GII-08` 的 C/D 覆盖。为避免「`GII-08` 等 `GII-07` 完成、`GII-07` 等 `GII-08` 的 C/D」的循环，`GII-08` 对 `GII-07` 的前驱条件定义为：`GII-07` 已达 `Acceptance=locally-accepted`、其 ER-05 review 已 `PASS`，且其本仓库改动已形成独立的本地签名 + DCO 提交（`Lifecycle` 保持 `in-progress`；按 `GII-07` 补偿动作① 就地修正时可追加提交）。每个本卡提交只含 `GII-07` `Deliverables` 中的本仓库文件：提交前 `libra status --porcelain` 暂存区为空，`libra add <本次修改的这些文件>` 后暂存路径集合恰为本次修改的文件，提交后 `libra show --name-only --format=format: HEAD` 恰为同一集合；本卡提交不单独推送。`GII-08` 在其之上提交，`GII-08` 的 C 组把本卡提交与 `GII-08` 的提交以同一 PR 推送，收口测试门被测树包含两者；`GII-08` 取得 C 组覆盖时 `GII-07` 同步进入 `remote-pending`，`GII-08` 的 D 组全绿时两卡同步置 `complete` / `done`；`GII-08` 若 `blocked`，`GII-07` 随之保持未完成；`GII-07` 处于 `blocked` 时不得随 `GII-08` 的 C/D 进入 `done`（ER-04：`blocked` 须先回到 `in-progress`）。`GII-07` 的网站改动（`libra-backend` 独立仓库）在 `GII-07` 内按「文档与网站验收命令」完成；修正或撤回按 `GII-07` 的补偿动作（ER-10 前滚）处理，不另设承载规则。
- 跨计划：每张卡另依赖 `DEP-GII-03`（开工编辑前满足，见「依赖登记表」）；它只门控该卡开工，不改变上述链序。

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-GII-01 | incoming | 跨计划写锁 | `issues/529#FP-13` | `#529` 执行者 | `issues/529.md` 释放 `src/cli.rs` 写锁 | `docs/development/plan/issues/529.md`（2026-11-17 核对；2026-11-18 复核 `FP-13` 仍为 `pending`） | 未释放前 `GII-01` 保持 `pending` |
| DEP-GII-02 | incoming | 用户授权 | 网站交付检出 `/Volumes/Data/gitmono/libra-backend`（`cf`）与 `AGENTS.md:135` 所写 `/Volumes/Sea/libra-backend` 不一致 | 用户 | **已满足：**用户 2026-11-18 11:55:09 UTC 确认本批计划的网站文档交付检出为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支），且本批不修改 `AGENTS.md` | 协调者转达的用户确认（2026-11-18 11:55:09 UTC）；`AGENTS.md:135` 的差异登记为已知文档滞后（「与其它计划的关系」） | 已满足，不构成任何任务卡或 Phase 的前置条件 |
| DEP-GII-03 | incoming | 跨计划共享写集串行 | 同批 `plan-20261117-*.md` 计划中与本卡 `Implementation write set` 相交的在途卡（`COMPATIBILITY.md` 为全部同批计划共享；网站第 4 步改写的生成产物 `apps/tanstack-app/src/generated/docs-manifest.ts`、`docs-search.ts` 与 `apps/tanstack-app/dist/` 视为与所有同批计划的网站交付相交；源码、测试、命令文档与网站页的交集以开工当日各计划写集为准） | 仓库协调者 | 每张卡开工编辑前，以 `plan-status.md` 与同批各计划的 `Implementation write set` 核对交集：无交集即满足；有交集时须取得协调者对相交文件的单写者串行窗口，窗口在本卡提交合入 `main` 后释放（`GII-07` 的窗口延续至其承载卡 `GII-08` 合入 `main` 后与 `GII-08` 一并释放） | 核对结果、交集文件与协调者放行记录写入该卡 `Verification` 记录 | 未满足前该卡保持 `pending`，不编辑相交文件（G-10）；超过 7 日未获窗口即报告协调者，不得并发写入 |

### 发布分组与并发窗口

- **并发声明:** 全串行（`GII-01`..`GII-24` 单链推进，实现与发布均不并发，彻底消除共享 `COMPATIBILITY.md` / `config.rs` / `cli.rs` / `snapshot.rs` / `client_storage.rs` 写集重叠，G-10 / ER-12）。
- **发布者:** 单一指定发布者（协调者指派的执行 Agent）。
- **发布窗口顺序:** 按各卡完成顺序串行执行 `patch + 1` 与 `gh release create`，同一时刻仅允许一张卡处于已 bump 未发布状态。

### Phase 0: 全局 CLI 选项与别名（GII-01..GII-05、GII-19、GII-24）

**目标:** 落地全局 `-C`、`--git-dir`/`--work-tree`、`GIT_DIR`/`GIT_WORK_TREE`、`--bare` 与 `alias.*` 展开。

**进入条件:** `DEP-GII-01` 满足。

**退出条件:** `GII-01`..`GII-05`、`GII-19` 与 `GII-24` 全部 `done` / `complete`（正式登记 `DEFER-*` 的卡除外，与「完成判据」的「所有非延后任务」一致）。

### Phase 1: `init` 兼容与格式校验（GII-06..GII-09、GII-20、GII-22）

**目标:** 修复 `init --template` 缺失目录告警、核实 `--shared=0666` 已符合 Git（`audit`）、重初始化 `"ignored --initial-branch"` 文案及 `repositoryformatversion`/`extensions.*` 校验。

**进入条件:** `GII-05` 完成（串行链前驱；`init.rs` 已由 `GII-04` 释放）。

**退出条件:** `GII-06`..`GII-09`、`GII-20` 与 `GII-22` 全部 `done` / `complete`（正式登记 `DEFER-*` 的卡除外，与「完成判据」的「所有非延后任务」一致）。

### Phase 2: `status`、修订、冲突、快照与基础设施（GII-10..GII-18、GII-21、GII-23）

**目标:** 落地 `status -v`、`status` 与 operation snapshot 扫描对前导符号链接（含互指循环）的处理（消除 `ELOOP` 中止）、`<rev>...` 范围与单一起点解析、嵌套多路冲突修复、`operation snapshot` 冲突修复、cloud object-index 队列修复、`add` 批量回放与 `fsck` 就地哈希性能优化及 Hermes extraction 下沉，完成全量收口门。

**进入条件:** `GII-20` 完成（串行链前驱）。

**退出条件:** 全部 24 张卡 `done` / `complete`（正式登记 `DEFER-*` 的卡除外，与「完成判据」的「所有非延后任务」一致），全量收口门全绿。

## 任务卡

### 任务卡粒度规则（强制）

继承 [plan-template.md](plan-template.md) 的 `G-01`..`G-11`。本计划的 G-03 计数口径：每条 `Acceptance criteria` 只陈述一条可独立失败的行为谓词与一个规范场景；「AC → 断言映射」只写判定该谓词所需的命令、退出码、输出与前后状态，不得出现 AC 之外的独立行为断言；不同语法形式、不同期望结果、不同触发条件或不同仓库形态一律拆为独立 AC。不设多输入「矩阵」例外：同一判定需对多个输入（如多个非法状态、多种对象格式）分别成立时，每个输入各计一条 AC；同一谓词在一组同类路径或对象上的全称断言（如「本批每个 blob 均有对应行」「三个冲突文件中的每条外层标记长度为 7」）计为一条，其映射须覆盖该组全部成员（枚举或以集合比较）；对不同路径或对象施加不同谓词、或分属不同场景时各计一条；对单一输出与映射中写全的期望文本做一次逐字相等比较计为一条。到达被判定状态所需的前置命令（如修复命令、合并命令），以及退出码不是该 AC 判定对象时被测命令本身的「退出 0」，都是该 AC 的执行条件（不满足即该 AC 失败），不另计；凡两个可分别失败的状态事实（如「推进 A 且不改变 B」）一律拆为两条 AC。失败类 AC 只断言稳定错误码（stderr 含 `Error-Code: <code>`），不断言具体数值退出码（数值退出码可被 `LIBRA_FINE_EXIT_CODES` 等开关改变），但「被测命令以非 0 退出」是每条失败类 AC 的执行条件，用例必须断言（映射未逐条重复此条件时亦适用）；需要断言失败文案的 AC 以「命令以非 0 退出」为执行条件、只判 stderr 文案。ER-06/ER-06a 同卡文档门不计入（G-03）。

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

**默认覆盖:** `GII-15`、`GII-16`、`GII-17`、`GII-18` 的 `Docs and compatibility impact` 为 `N/A`（纯内部修复/性能/守卫，无公开 CLI 参数或输出变更）；`GII-07` 取 `Task type=audit`、`Release boundary=no-release`、`Rollback mode=compensating`、`Version increment=N/A`，C/D 覆盖继承 `GII-08`（承载时序见「实施顺序」承载例外）；`GII-16`、`GII-18` 另在卡内展开 `Performance budget`（基准阈值）。

**规则 waiver（`EX-*`）:** 无。

### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| GII-01 | implementation | 全局 `-C <path>` 目录切换 | immutable-release | yes | yes | 8/8 | 1/8 | 2/3 | M | DEP-GII-01, DEP-GII-03 | 序列化于 DEP-GII-01 | independent | N/A | N/A |
| GII-02 | implementation | 全局 `--git-dir=`/`--work-tree=` 等号形式 | immutable-release | yes | yes | 7/8 | 1/8 | 3/4 | M | GII-19, DEP-GII-03 | 序列化于 GII-19 | independent | N/A | N/A |
| GII-03 | implementation | `GIT_DIR`/`GIT_WORK_TREE` 环境变量 | immutable-release | yes | yes | 7/8 | 1/8 | 2/3 | M | GII-24, DEP-GII-03 | 序列化于 GII-24 | independent | N/A | N/A |
| GII-04 | implementation | 全局 `--bare` | immutable-release | yes | yes | 6/8 | 1/8 | 4/4 | M | GII-03, DEP-GII-03 | 序列化于 GII-03 | independent | N/A | N/A |
| GII-05 | implementation | `alias.*` Git 别名展开 | immutable-release | yes | yes | 8/8 | 1/8 | 3/4 | M | GII-04, DEP-GII-03 | 序列化于 GII-04 | independent | N/A | N/A |
| GII-06 | implementation | `init --template` 缺失目录告警 | immutable-release | yes | yes | 7/8 | 1/8 | 1/1 | M | GII-05, DEP-GII-03 | 序列化于 GII-05 | independent | N/A | N/A |
| GII-07 | audit | 核实 `init --shared=0666` 并收口 #943 | compensating | yes | yes | 20/20 | 9/20 | 0/0 | S | GII-06, DEP-GII-03 | 序列化于 GII-06 | no-release | N/A | N/A |
| GII-08 | implementation | `init` `ignored --initial-branch` 文案 | immutable-release | yes | yes | 2/8 | 1/8 | 1/1 | M | GII-07, DEP-GII-03 | 序列化于 GII-07 | independent | N/A | N/A |
| GII-09 | implementation | `repositoryformatversion` 与扩展存在性校验 | immutable-release | yes | yes | 8/8 | 1/8 | 3/3 | M | GII-08, DEP-GII-03 | 序列化于 GII-08 | independent | N/A | N/A |
| GII-10 | implementation | `status -v`/`--verbose` | immutable-release | yes | yes | 7/8 | 1/8 | 2/3 | M | GII-20, DEP-GII-03 | 序列化于 GII-20 | independent | N/A | N/A |
| GII-11 | implementation | `status` 扫描前导符号链接 | immutable-release | yes | yes | 3/8 | 1/8 | 2/3 | M | GII-10, DEP-GII-03 | 序列化于 GII-10 | independent | N/A | N/A |
| GII-12 | implementation | `<rev>...` 三点范围解析（`rev-parse`） | immutable-release | yes | yes | 5/8 | 1/8 | 2/2 | M | GII-21, DEP-GII-03 | 序列化于 GII-21 | independent | N/A | N/A |
| GII-13 | implementation | 嵌套多路冲突标记宽度与 ours 行保留 | immutable-release | yes | yes | 3/8 | 1/8 | 1/1 | M | GII-23, DEP-GII-03 | 序列化于 GII-23 | independent | N/A | N/A |
| GII-14 | implementation | `mv` 目标越过符号链接拒绝 | immutable-release | yes | yes | 8/8 | 2/8 | 1/1 | M | GII-13, DEP-GII-03 | 序列化于 GII-13 | independent | N/A | N/A |
| GII-15 | implementation | 仓库数据库繁忙时存储类型解析 | immutable-release | yes | yes | 6/8 | 3/8 | 2/2 | S | GII-14, DEP-GII-03 | 序列化于 GII-14 | independent | N/A | N/A |
| GII-16 | implementation | `add` object-index 批末批量回放 | immutable-release | yes | yes | 8/8 | 3/8 | 2/2 | M | GII-15, DEP-GII-03 | 序列化于 GII-15 | independent | N/A | N/A |
| GII-17 | implementation | Hermes extraction 下沉至 `observed_agents` | immutable-release | yes | yes | 7/8 | 5/8 | 2/2 | S | GII-16, DEP-GII-03 | 序列化于 GII-16 | independent | N/A | N/A |
| GII-18 | implementation | `fsck` 对象校验就地哈希 | immutable-release | yes | yes | 7/8 | 3/8 | 1/1 | S | GII-17, DEP-GII-03 | 序列化于 GII-17 | independent | GII-16 | N/A |
| GII-19 | implementation | 全局预扫描：重复 `-C`、与既有标志交错及 `--` 透传 | immutable-release | yes | yes | 7/8 | 1/8 | 2/3 | M | GII-01, DEP-GII-03 | 序列化于 GII-01 | independent | GII-01 | N/A |
| GII-20 | implementation | `extensions.refstorage` 校验 | immutable-release | yes | yes | 8/8 | 1/8 | 2/2 | M | GII-22, DEP-GII-03 | 序列化于 GII-22 | independent | GII-09 | N/A |
| GII-21 | implementation | operation snapshot 扫描前导符号链接 | immutable-release | yes | yes | 8/8 | 3/8 | 1/1 | M | GII-11, DEP-GII-03 | 序列化于 GII-11 | independent | GII-11 | N/A |
| GII-22 | implementation | `extensions.objectformat` 取值与一致性校验 | immutable-release | yes | yes | 6/8 | 1/8 | 2/2 | M | GII-09, DEP-GII-03 | 序列化于 GII-09 | independent | GII-09 | N/A |
| GII-23 | implementation | `checkout -b A...B` 单一起点 merge-base 解析 | immutable-release | yes | yes | 8/8 | 1/8 | 2/2 | M | GII-12, DEP-GII-03 | 序列化于 GII-12 | independent | GII-12 | N/A |
| GII-24 | implementation | 全局 `--git-dir`/`--work-tree` 空格分隔形式与缺值 | immutable-release | yes | yes | 5/8 | 1/8 | 2/3 | M | GII-02, DEP-GII-03 | 序列化于 GII-02 | independent | GII-02 | N/A |

---

### Task GII-01: 支持全局 `-C <path>` 目录切换（#686）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在全局 CLI 入口支持位于 argv 首位的 `-C <path>`（及粘连形式 `-C<path>`）：在仓库发现与子命令执行前切换工作目录，并把已消费的 `-C <path>` 从交给 `OutputConfig::resolve_from_argv`（含错误渲染路径 `src/main.rs:699-703` 所用的 argv）与 `clap` 的 argv 中移除，使路径值不会被误判为输出标志（修复 t3200#95 等 `git -C` 工作流中止问题）；空路径 `-C ''` 按 Git 文档保持当前目录不变。本卡发布中预扫描只消费首位的单个 `-C`：非首位的 `-C`（位于既有全局标志或另一个 `-C` 之后）不注册进 clap、原样交给 clap，按当前行为以 `LBR-CLI-002` 拒绝（发布文档写明「本版只支持 argv 首位的单个 `-C`」的限制）；重复 `-C` 与交错形式由 `GII-19` 解除（2026-11-18 按 G-09 拆出）。

**Out of scope:** 重复 `-C`、`-C` 与既有全局标志任意交错、子命令 `--` 之后参数不触发输出标志（由 `GII-19` 承接）；`--git-dir` / `--work-tree`（由 `GII-02`、`GII-24` 承接）；`-c <key>=<value>` 全局配置覆盖（#557，不在本计划跟踪范围，尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 全局 `Cli` 参数（`src/cli.rs:234-336`）未注册 `-C`；已安装 0.30.207 实测 `libra -C <dir> status` → `error: unexpected argument '-C' found`、`Error-Code: LBR-CLI-002` | `src/cli.rs:234-336`、#686 |
| `OutputConfig::resolve_from_argv` 扫描整个原始 argv（不区分参数值与 `--`），遇到 `--json` 即进入 JSON 模式 | `src/utils/output.rs:250-327` |

**Acceptance criteria:**

- [ ] AC1：分隔形式 `libra -C <dir> status` 在 `<dir>` 内执行。
- [ ] AC2：粘连形式 `libra -C<dir> status` 在 `<dir>` 内执行。
- [ ] AC3：非首位的 `-C`（规范场景：位于既有全局标志之后的 `libra --no-pager -C <dir> status`）以 `LBR-CLI-002` 失败（本卡发布的已文档化限制）。
- [ ] AC4：子命令 `--` 之后的 `-C` 作为子命令参数处理，不切换目录。
- [ ] AC5：`--json` 作为 `-C` 的目录名且命令失败时，错误以人读格式输出（不进入 JSON 模式）。
- [ ] AC6：空路径 `libra -C '' status` 不改变当前目录（Git 语义）。
- [ ] AC7：末尾缺值的 `libra -C` 以 `LBR-CLI-002` 失败。
- [ ] AC8：目标目录不存在时 `libra -C <missing> status` 以 `LBR-IO-001` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`、`COMPATIBILITY.md` 及 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::init_test::global_dash_c_directory` 承载；夹具：在 `<tmp>/r` 内以普通 `libra init` 建库并提交一次，再创建未跟踪文件 `u.txt` 与名为 `-C` 的未跟踪文件；`<tmp>/outside` 为不含仓库的目录；基线 `B` = 在 `<tmp>/r` 内直接运行 `libra status --short` 的 stdout）

- AC1：cwd 为 `<tmp>/outside`，`libra -C <tmp>/r status --short` 退出 0，stdout 等于 `B`。
- AC2：cwd 为 `<tmp>/outside`，`libra -C<tmp>/r status --short` 退出 0，stdout 等于 `B`。
- AC3：cwd 为 `<tmp>/outside`，`libra --no-pager -C <tmp>/r status --short` 的 stderr 含 `Error-Code: LBR-CLI-002`（把 `-C` 注册进 clap 却未在该位置切换目录的错误实现会在 `<tmp>/outside` 执行并报 `LBR-REPO-001`）。
- AC4：cwd 为 `<tmp>/r`，`libra status --short --no-branch -- -C` 退出 0，stdout 恰为 `?? -C` 一行（`-C` 作为 pathspec 匹配到当前仓库中的该文件）。
- AC5：`<tmp>/--json` 为不含仓库的空目录；cwd 为 `<tmp>`，`libra -C --json status` 的 stderr 含人读错误标记 `Error-Code: LBR-REPO-001`（成功路径按 clap 解析结果决定输出模式，`src/cli.rs:3780-3788`，而错误路径以原始 argv 解析，`src/main.rs:699-703`；未从错误渲染所用 argv 中剔除 `-C` 值的错误实现会输出不含该标记的 JSON 错误信封，`src/utils/error.rs:1142-1160`）。
- AC6：cwd 为 `<tmp>/r`，`libra -C '' status --short` 退出 0，stdout 等于 `B`。
- AC7：cwd 为 `<tmp>/outside`，`libra -C` 的 stderr 含 `Error-Code: LBR-CLI-002`。
- AC8：cwd 为 `<tmp>/outside`，`libra -C <tmp>/missing status` 的 stderr 含 `Error-Code: LBR-IO-001`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::global_dash_c_directory)'`（`(new)`；按「验证执行协议」：`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 触及 src/cli.rs 全局 flag 注册及版本面集合` | **Dependencies:** `DEP-GII-01, DEP-GII-03`

**Implementation write set:** `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `tests/command/init_test.rs`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `tests/command/init_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`（`## Global Flags` / `## 全局标志` 记录 `-C`，含空路径不切换目录，以及「本版只支持 argv 首位的单个 `-C`」的限制）、`COMPATIBILITY.md`（原地更新既有行，不改行数）与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags` 记录 `-C`）；`docs/development/commands/README.md` 为命令索引、无全局标志章节，`N/A`。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=全局-C目录切换; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=DEP-GII-01,DEP-GII-03; writeset=序列化于DEP-GII-01; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-02: 支持全局 `--git-dir=<path>` 与 `--work-tree=<path>`（等号形式；#801）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在全局 CLI 入口支持等号形式 `--git-dir=<path>` 与 `--work-tree=<path>`，覆盖默认 `.libra` 仓库存储目录与工作树根目录的发现，并把已消费的标志从交给 `OutputConfig::resolve_from_argv`（含错误渲染路径 `src/main.rs:699-703` 所用的 argv）与 `clap` 的 argv 中移除；空值按 Libra 的 fail-closed 取值以 `LBR-CLI-002` 拒绝（按 GC-03 写入文档）。本卡发布中只支持等号形式：空格分隔形式 `--git-dir <path>` / `--work-tree <path>` 不被预扫描消费、也不注册进 clap，原样交给 clap 并按当前行为以 `LBR-CLI-002` 拒绝（发布文档写明「本版只支持等号形式」的限制）；空格分隔形式及其末尾缺值由 `GII-24` 解除与定义（2026-11-18 按 G-09 拆出）。

**Out of scope:** 空格分隔形式、以 `-` 开头的取值（如 `--git-dir --json`）与末尾缺值（`GII-24`）；`GIT_DIR` / `GIT_WORK_TREE` 环境变量（由 `GII-03` 承接）；子命令 `--` 之后参数不触发输出标志（`GII-19` 已交付，本卡共享回归覆盖）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 全局 `Cli` 参数（`src/cli.rs:234-336`）未注册 `--git-dir` / `--work-tree`；仓库发现 `try_get_storage_path` / `storage_path` / `working_dir` 只从 cwd 向上查找 `.libra` | `src/cli.rs:234-336`、`src/utils/util.rs:1262-1269,1464`、#801 |

**Acceptance criteria:**

- [ ] AC1：等号形式 `libra --git-dir=<r>/.libra --work-tree=<w> status`（`<w>` 不是 `<r>`）在 cwd 为无关目录时以 `<r>` 的仓库数据与 `<w>` 的工作树执行。
- [ ] AC2：空格分隔形式（本卡发布的已文档化限制，规范场景 `libra --git-dir <r>/.libra status`）以 `LBR-CLI-002` 失败。
- [ ] AC3：`--git-dir` 指向不存在的路径时以 `LBR-REPO-001` 失败。
- [ ] AC4：`--work-tree` 指向不存在路径时以 `LBR-IO-001` 失败。
- [ ] AC5：空值 `--git-dir=` 以 `LBR-CLI-002` 失败。
- [ ] AC6：空值 `--work-tree=` 以 `LBR-CLI-002` 失败。
- [ ] AC7：`--work-tree` 的空格分隔形式（本卡发布的已文档化限制，规范场景 `libra --git-dir=<r>/.libra --work-tree <w> status`）以 `LBR-CLI-002` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::status_test::global_git_dir_and_work_tree` 承载；子进程 cwd 为无仓库的 `<tmp>/outside`；`<r>` 的唯一提交只含 `a.txt`，`<r>` 另含未跟踪文件 `u.txt`；`<w>` 为独立目录，只含与该提交内容相同的 `a.txt` 和未跟踪文件 `w.txt`；忽略 `--work-tree`、改以 `<r>` 为工作树的错误实现会输出 `?? u.txt`）

- AC1：`libra --git-dir=<r>/.libra --work-tree=<w> status --short --no-branch` 退出 0，stdout 恰为 `?? w.txt` 一行。
- AC2：`libra --git-dir <r>/.libra status` 的 stderr 含 `Error-Code: LBR-CLI-002`（把 `--git-dir` 注册进 clap 却未切换仓库的错误实现会在 `<tmp>/outside` 执行并报 `LBR-REPO-001`）。
- AC3：`libra --git-dir=<tmp>/none status` 的 stderr 含 `Error-Code: LBR-REPO-001`。
- AC4：`libra --git-dir=<r>/.libra --work-tree=<tmp>/none status` 的 stderr 含 `Error-Code: LBR-IO-001`。
- AC5：`libra --git-dir= status` 的 stderr 含 `Error-Code: LBR-CLI-002`。
- AC6：`libra --git-dir=<r>/.libra --work-tree= status` 的 stderr 含 `Error-Code: LBR-CLI-002`（把空工作树当作当前目录的错误实现会在 `<tmp>/outside` 执行而不报该错误码）。
- AC7：`libra --git-dir=<r>/.libra --work-tree <w> status` 的 stderr 含 `Error-Code: LBR-CLI-002`（意外接受空格分隔 `--work-tree` 的实现会以 `<w>` 为工作树成功执行）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::status_test::global_git_dir_and_work_tree)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::/) or test(/^utils::util::test::/)'`（按「验证执行协议」第 5 条；不计为 AC 证据）

**Full-suite trigger:** `T-1: 触及 src/cli.rs 全局 flag 注册及版本面集合` | **Dependencies:** `GII-19, DEP-GII-03`

**Implementation write set:** `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `src/utils/util.rs`, `tests/command/status_test.rs`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `src/utils/util.rs`, `tests/command/status_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`（全局标志章节记录等号形式的 `--git-dir` / `--work-tree`、空值拒绝及「本版只支持等号形式」的限制）、`COMPATIBILITY.md`（原地更新既有行，不改行数）与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`）；`docs/development/commands/README.md` 无全局标志章节，`N/A`。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=全局--git-dir=/--work-tree=等号形式; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=3; prod-files=4; scope=M; deps=GII-19,DEP-GII-03; writeset=序列化于GII-19; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-24: 全局 `--git-dir` / `--work-tree` 空格分隔形式与末尾缺值（#801，自 GII-02 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 解除 `GII-02` 发布中「本版只支持等号形式」的限制：全局预扫描识别空格分隔形式 `--git-dir <path>` 与 `--work-tree <path>`，按 `ADR-GII-01` 把紧随其后的下一 token 作为取值（即使以 `-` 开头，如 `--git-dir --json`，与 Git 一致），并把标志与取值从交给 `OutputConfig::resolve_from_argv`（含错误渲染路径 `src/main.rs:699-703` 所用的 argv）与 `clap` 的 argv 中剔除；标志位于 argv 末尾缺值时以 `LBR-CLI-002` 失败。拆分原因（G-09）：原 `GII-02` 按独立谓词计数超过 G-03 上限；等号形式与空格分隔形式是两个可独立恢复的轴，`GII-02` 单独发布时空格分隔形式按现状拒绝且有文档说明，本卡追加于 Phase 0 末尾、在执行链上紧随 `GII-02`。

**Out of scope:** 等号形式与空值（`GII-02`）；`GIT_DIR` / `GIT_WORK_TREE`（`GII-03`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `GII-02` 交付后空格分隔形式仍以 `LBR-CLI-002` 拒绝 | 本计划 `GII-02` AC2 |
| 错误渲染路径以原始进程 argv 解析输出模式 | `src/main.rs:699-703` |

**Acceptance criteria:**

- [ ] AC1：空格分隔形式 `libra --git-dir <r>/.libra --work-tree <w> status` 在 cwd 为无关目录时以 `<w>` 为工作树执行。
- [ ] AC2：`--git-dir` 的取值以 `-` 开头时（`libra --git-dir --json status`）该取值按路径处理，失败时错误以人读格式输出（不进入 JSON 模式）。
- [ ] AC3：`--git-dir` 位于 argv 末尾缺值时以 `LBR-CLI-002` 失败。
- [ ] AC4：`--work-tree` 位于 argv 末尾缺值时以 `LBR-CLI-002` 失败。
- [ ] AC5：`--work-tree` 的取值以 `-` 开头时（`libra --git-dir=<r>/.libra --work-tree --json status`）该取值按路径处理，失败时错误以人读格式输出（不进入 JSON 模式）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::status_test::global_git_dir_and_work_tree_space_form` 承载；夹具与 `GII-02` 相同的构造在本用例内重建：子进程 cwd 为无仓库的 `<tmp>/outside`；`<r>` 的唯一提交只含 `a.txt`，另含未跟踪 `u.txt`；`<w>` 为独立目录，只含同内容的 `a.txt` 与未跟踪 `w.txt`）

- AC1：`libra --git-dir <r>/.libra --work-tree <w> status --short --no-branch` 退出 0，stdout 恰为 `?? w.txt` 一行（仍拒绝空格分隔形式或忽略 `--work-tree` 的实现不满足）。
- AC2：`libra --git-dir --json status` 的 stderr 含人读错误标记 `Error-Code: LBR-REPO-001`（`--json` 作为不存在的仓库路径；错误路径以原始 argv 解析输出模式而未剔除该取值的错误实现会输出不含该标记的 JSON 错误信封，`src/utils/error.rs:1142-1160`）。
- AC3：`libra --git-dir` 的 stderr 含 `Error-Code: LBR-CLI-002`。
- AC4：`libra --git-dir=<r>/.libra --work-tree` 的 stderr 含 `Error-Code: LBR-CLI-002`。
- AC5：`libra --git-dir=<r>/.libra --work-tree --json status` 的 stderr 含人读错误标记 `Error-Code: LBR-IO-001`（`--json` 作为不存在的工作树路径；把它当作输出标志的错误实现会输出不含该标记的 JSON 错误信封，或把 `status` 当作工作树路径而报其它错误）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::status_test::global_git_dir_and_work_tree_space_form)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::/)'`（按「验证执行协议」第 5 条；不计为 AC 证据）

**Full-suite trigger:** `T-1: 触及 src/cli.rs 全局 flag 预扫描及版本面集合` | **Dependencies:** `GII-02, DEP-GII-03`

**Implementation write set:** `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `tests/command/status_test.rs`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `tests/command/status_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`（全局标志章节删除「本版只支持等号形式」的限制，说明空格分隔形式的取值即下一 token 与末尾缺值拒绝）、`COMPATIBILITY.md`（原地更新既有行，不改行数）与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`）；`docs/development/commands/README.md` 无全局标志章节，`N/A`。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=全局--git-dir/--work-tree空格分隔形式与缺值; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=GII-02,DEP-GII-03; writeset=序列化于GII-02; release=independent; split-from=GII-02; exception=N/A`

---

### Task GII-03: 支持 `GIT_DIR` / `GIT_WORK_TREE` 环境变量（#987）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在仓库发现管线（`src/utils/util.rs` 与 `src/cli.rs` / `src/main.rs` 预处理）识别 `GIT_DIR` 与 `GIT_WORK_TREE` 环境变量（优先级低于显式 `--git-dir` / `--work-tree`），不再静默回落操作 cwd 仓库。

**Out of scope:** 全局 `--bare`（由 `GII-04` 承接）；`GIT_COMMON_DIR` / `GIT_INDEX_FILE` 等其它 Git 环境变量（尚未排期，重启条件：出现对应上游用例或 issue）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 仓库发现不读取 `GIT_DIR`，直接从 cwd 向上查找 `.libra`；已安装 0.30.207 实测 `GIT_DIR=/nonexistent libra status` 仍输出 cwd 仓库状态并退出 0 | `src/utils/util.rs:1262-1269,1464`、#987 |

**Acceptance criteria:**

- [ ] AC1：设置 `GIT_DIR=<r>/.libra`、`GIT_WORK_TREE=<r>` 时，`libra rev-parse HEAD` 作用于 `<r>` 而非 cwd 所在仓库 `<c>`。
- [ ] AC2：同一环境下写命令 `commit --allow-empty` 推进 `<r>` 的 `HEAD`。
- [ ] AC3：该写命令不改变 cwd 所在仓库 `<c>` 的 `HEAD`。
- [ ] AC4：显式 `--git-dir` 优先于 `GIT_DIR`。
- [ ] AC5：`GIT_DIR` 指向不存在的仓库时以 `LBR-REPO-001` 失败（不回落 cwd 仓库）。
- [ ] AC6：设置 `GIT_DIR=<r>/.libra`、`GIT_WORK_TREE=<w>` 时 `libra status --short` 以 `<w>` 为工作树。
- [ ] AC7：显式 `--work-tree` 优先于 `GIT_WORK_TREE`。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags` 说明 `GIT_DIR` / `GIT_WORK_TREE`），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::status_test::git_dir_env_overrides_cwd` 承载；环境变量只注入子进程，不修改测试进程环境，故无需 `#[serial(env)]`；子进程 cwd 位于仓库 `<c>` 内，`<c>`、`<r>`、`<s>` 的首个提交 `c1` / `r1` / `s1` 均以 `libra commit --allow-empty` 创建（不含文件、彼此不同），`<r>` 含未跟踪文件 `u.txt`；`<w>` 为独立目录，只含未跟踪文件 `w.txt`）

- AC1：`libra rev-parse HEAD` 退出 0 且 stdout 为 `r1`。
- AC2：`libra commit --allow-empty -m r2` 须退出 0（执行条件）；`<r>` 的 `HEAD` 的父提交为 `r1`。
- AC3：在 AC2 的命令之前与之后，以不含 `GIT_DIR` / `GIT_WORK_TREE` 的子进程环境各取 `<c>` 的 HEAD 快照（`libra symbolic-ref HEAD` 的 stdout 与 `libra rev-parse HEAD` 的 stdout 拼接，cwd 为 `<c>`），前后逐字节相等（把 `<c>` 的符号 HEAD 切到另一同样指向 `c1` 的分支的错误实现会被识别）。
- AC4：再设 `GIT_DIR=<r>/.libra` 并传 `--git-dir=<s>/.libra --work-tree=<s>`，`libra rev-parse HEAD` 退出 0 且输出 `s1`。
- AC5：`GIT_DIR=<tmp>/none libra status` 的 stderr 含 `Error-Code: LBR-REPO-001`（回落到 `<c>` 的错误实现会成功执行，不产生该错误码）。
- AC6：`GIT_DIR=<r>/.libra GIT_WORK_TREE=<w> libra status --short --no-branch` 退出 0，stdout 恰为 `?? w.txt` 一行（忽略 `GIT_WORK_TREE` 的错误实现输出 `?? u.txt`）。
- AC7：同一环境下 `libra --work-tree=<r> status --short --no-branch` 退出 0，stdout 恰为 `?? u.txt` 一行（让环境变量覆盖显式参数的错误实现输出 `?? w.txt`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::status_test::git_dir_env_overrides_cwd)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::/) or test(/^utils::util::test::/)'`（按「验证执行协议」第 5 条；不计为 AC 证据）

**Full-suite trigger:** `T-1: 触及 src/cli.rs / src/utils/util.rs 共享仓库发现及版本面集合` | **Dependencies:** `GII-24, DEP-GII-03`

**Implementation write set:** `src/cli.rs`, `src/main.rs`, `src/utils/util.rs`, `tests/command/status_test.rs`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/main.rs`, `src/utils/util.rs`, `tests/command/status_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`（全局标志章节说明 `GIT_DIR` / `GIT_WORK_TREE` 及其与 `--git-dir` / `--work-tree` 的优先级）、`COMPATIBILITY.md`（原地更新既有行，不改行数）与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`）；`docs/development/commands/README.md` 无全局标志章节，`N/A`。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=GIT_DIR/GIT_WORK_TREE环境变量; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=GII-24,DEP-GII-03; writeset=序列化于GII-24; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-04: 支持全局 `--bare` 选项（#944）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在全局 `Cli` 入口支持 Git 全局 `--bare`：`libra --bare init <dir>` 等价于 `libra init --bare <dir>`；其它子命令按 Git 语义把当前目录视为仓库存储目录并以裸仓库上下文运行。该上下文只对本次调用生效、不写入 `core.bare`：裸仓库判定处（如 `rev-parse --is-bare-repository` 现读持久化的 `core.bare`，`src/command/rev_parse.rs:900-914`）与「须在工作树中运行」的检查先看本次调用的裸上下文。

**Out of scope:** `init --template` / `--shared`（分别由 `GII-06`、`GII-07` 承接）；`--bare` 之外的 `GIT_DIR` 覆盖（`GII-03`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--bare` 只注册在 `init` 子命令（`InitArgs.bare`，`src/command/init.rs:336-340`），全局参数（`src/cli.rs:234-336`）无 `--bare`；已安装 0.30.207 实测 `libra --bare init b1` → `error: unexpected argument '--bare' found`、`LBR-CLI-002` | `src/command/init.rs:336-340`、`src/cli.rs:234-336`、#944 |
| `rev-parse --is-bare-repository` 已存在 | `src/command/rev_parse.rs:110-111` |

**Acceptance criteria:**

- [ ] AC1：`libra --bare init <dir>` 生成与 `libra init --bare <dir2>` 相同的顶层布局。
- [ ] AC2：`libra --bare init <dir>` 生成的仓库 `core.bare` 为 `true`。
- [ ] AC3：在非裸仓库的存储目录 `<r>/.libra` 内运行 `libra --bare rev-parse --is-bare-repository` 输出 `true`。
- [ ] AC4：在非存储目录（如工作树根 `<r>`）运行 `libra --bare status` 以 `LBR-REPO-001` 失败。
- [ ] AC5：在非裸仓库的存储目录 `<r>/.libra` 内运行 `libra --bare status` 以 `LBR-REPO-003` 失败（以裸仓库上下文运行，无工作树）。
- [ ] AC6：AC3 与 AC5 的调用不持久化改动该仓库的配置。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/{README,init}.md`、`docs/commands/zh-CN/{README,init}.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md` 与网站 `index.en.mdx`、`init.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::init_test::global_bare_init` 承载）

- AC1：`libra --bare init <tmp>/b1` 与 `libra init --bare <tmp>/b2` 均退出 0；两目录顶层条目名集合相同（只比较名称，不比较内容随机的密钥/数据库字节）。
- AC2：`libra -C <tmp>/b1 config get core.bare` 退出 0，stdout 为 `true`（`-C` 由前置卡 `GII-01` 交付）。
- AC3：在 `<r>/.libra` 内运行，退出 0，stdout 恰为 `true`。
- AC4：stderr 含 `Error-Code: LBR-REPO-001`。
- AC5：在 `<r>/.libra` 内运行 `libra --bare status`，stderr 含 `Error-Code: LBR-REPO-003`（不带 `--bare` 时同一目录中的 `libra status` 作用于 `<r>` 并成功——已安装 0.30.207 实测退出 0；裸仓库中的 `status` 以 `LBR-REPO-003`「this operation must be run in a work tree」失败——同版本实测；忽略 `--bare` 的错误实现会成功）。
- AC6：AC3 之前与 AC5 之后，以只读 SQLite 连接读取 `<r>/.libra/libra.db` 中 `config` 与 `config_kv` 两表的全部行（含全部列、按 `id` 排序，`sql/sqlite_20260309_init.sql:5-17`），前后逐行相等（比较存储值本身，加密值的变化同样可识别；把 `core.bare` 写成 `true` 来满足 AC3 的错误实现会被识别）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::global_bare_init)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::init_test::.*bare/)'`（2026-11-18 核对现有 6 条，如 `command::init_test::init_bare_reinit_tops_up_and_preserves_state`；按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 触及 src/cli.rs 全局 flag 注册及版本面集合` | **Dependencies:** `GII-03, DEP-GII-03`

**Implementation write set:** `src/cli.rs`, `src/command/init.rs`, `src/command/rev_parse.rs`, `src/utils/util.rs`, `tests/command/init_test.rs`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/command/init.rs`, `src/command/rev_parse.rs`, `src/utils/util.rs`, `tests/command/init_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/{README,init}.md`、`docs/commands/zh-CN/{README,init}.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md`（原地更新既有行，不改行数）与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags` 记录 `--bare`）；`init.en.md` 由 `sync-libra-command-docs.py --only init` 生成。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=全局--bare选项; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=4; prod-files=4; scope=M; deps=GII-03,DEP-GII-03; writeset=序列化于GII-03; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-05: 支持 `alias.*` Git 别名展开（#945）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `src/cli.rs` / `src/main.rs` 预处理阶段识别未匹配内置子命令（含 clap 内置别名）的首个子命令 token，经共享的严格级联读取函数 `read_cascaded_config_value_strict`（`src/internal/config.rs:1872`）读取 `alias.<name>`，以 POSIX 引号/转义规则分词展开、与剩余参数拼接后重新送入 CLI 解析；展开深度上限 16 防止循环，`!` shell 别名明确拒绝（`DEFER-GII-01`）。

**Out of scope:** 执行 `!<shell>` 外部命令别名（`DEFER-GII-01`，重启条件见该行）；`--` 之后参数不影响输出模式由 `GII-19` 交付的 `OutputConfig::resolve_from_argv` 规则提供，本卡只交付「该函数作用于展开后的 argv」（AC3），不另设 AC。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `src/cli.rs` / `src/main.rs` 不读取 `alias.*`，未知子命令由 `classify_parse_error` 报 `LBR-CLI-001` | `src/cli.rs:3176-3210`、`src/utils/error.rs:373` |
| clap 已有内置别名，如 `status` 的 `st`（另有 `br`、`ci`、`sw`、`cfg` 等） | `src/cli.rs:355` |

**Acceptance criteria:**

- [ ] AC1：仅在全局配置中设置 `alias.ss = "status --short"`（本地配置无该键）时，`libra ss` 的 stdout 与 `libra status --short` 的 stdout 逐字节相同（两者均须退出 0，属执行条件）。
- [ ] AC2：别名值按 POSIX shell 风格的引号与反斜杠转义规则分词，并把调用时的其余参数拼接在展开结果之后：别名值（字面量）`log --format='%h'\ %s` 在 `libra lg -1` 中展开为 `log`、`--format=%h %s`、`-1` 三个参数。
- [ ] AC3：`OutputConfig::resolve_from_argv` 作用于展开后的 argv：仅在全局配置中设置 `alias.sj = "status --json"` 时，在非仓库目录运行 `libra sj` 的错误以 JSON 错误信封输出（`error_code` 为 `LBR-REPO-001`）。
- [ ] AC4：循环别名（`alias.a = b`、`alias.b = a`，展开深度超过 16）以 `LBR-CLI-002` 失败。
- [ ] AC5：含未闭合引号的别名值以 `LBR-CLI-002` 失败。
- [ ] AC6：`!` 开头的 shell 别名以 `LBR-CLI-002` 失败（`DEFER-GII-01`）。
- [ ] AC7：`!` 开头的 shell 别名不被执行。
- [ ] AC8：别名不覆盖 clap 内置别名（匹配 Git 的内置优先）：设置 `alias.st = log` 后 `libra st` 仍执行内置 `status`。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/{README,config}.md`、`docs/commands/zh-CN/{README,config}.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md` 及网站 `config.en.md`、`index.en.mdx`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::config_test::git_alias_expansion` 承载；仓库 `<r>` 含一次提交与未跟踪文件 `u.txt`）

- AC1：子进程环境注入本用例隔离的 `HOME=<tmp>/home` 与 `LIBRA_CONFIG_GLOBAL_DB=<tmp>/home/global.db`（不修改测试进程环境），`libra config set --global alias.ss "status --short"` 须退出 0（执行条件），夹具从不写入本地 `alias.ss`；`libra ss` 与 `libra status --short` 均退出 0，stdout 逐字节相同（含 `?? u.txt`）。
- AC2：仓库 `<r>` 另有第二次提交（共 2 个提交）；`libra lg -1` 退出 0，stdout 恰为 HEAD 提交的 `<短 OID> <提交标题>` 一行（未处理单引号或反斜杠转义的实现会得到不同的格式参数，丢弃其余参数 `-1` 的实现会输出两行；一次逐字比较）。
- AC3：以与 AC1 相同的隔离全局配置执行 `libra config set --global alias.sj "status --json"`（执行条件）；cwd 为无仓库的 `<tmp>/outside`，`libra sj` 须以非 0 退出（执行条件），stderr 可被解析为 JSON 对象且其 `error_code` 字段为 `LBR-REPO-001`（错误信封由 `CliError::render_json` 生成，`src/utils/error.rs:1084-1090`；错误渲染以原始 argv `libra sj` 解析输出模式——`src/main.rs:699-703`——的错误实现输出人读文本，无法解析为 JSON）。
- AC4：`libra a` 的 stderr 含 `Error-Code: LBR-CLI-002`（不展开别名的错误实现报未知子命令 `LBR-CLI-001`）。
- AC5：`alias.bad = "log --format='%h"` 时 `libra bad` 的 stderr 含 `Error-Code: LBR-CLI-002`。
- AC6：`alias.sh = "!touch <tmp>/pwned"` 时 `libra sh` 的 stderr 含 `Error-Code: LBR-CLI-002`（把 `!touch` 当普通子命令的错误实现报 `LBR-CLI-001`）。
- AC7：AC6 那次 `libra sh` 之后 `<tmp>/pwned` 不存在（先执行 shell 命令再报错的错误实现会创建它）。
- AC8：`libra st` 退出 0，stdout 与未设别名时的 `libra status` 逐字节相同。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::config_test::git_alias_expansion)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::/)'`（按「验证执行协议」第 5 条；不计为 AC 证据）

**Full-suite trigger:** `T-1: 触及 src/cli.rs 命令分发及版本面集合` | **Dependencies:** `GII-04, DEP-GII-03`

**Implementation write set:** `src/cli.rs`, `src/main.rs`, `src/internal/config.rs`, `src/utils/output.rs`, `tests/command/config_test.rs`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/main.rs`, `src/internal/config.rs`, `src/utils/output.rs`, `tests/command/config_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/{README,config}.md`、`docs/commands/zh-CN/{README,config}.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`（原地更新既有行，不改行数）与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`；`config.en.md` 由 `sync-libra-command-docs.py --only config` 生成。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=alias.*_Git别名展开; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=3; prod-files=4; scope=M; deps=GII-04,DEP-GII-03; writeset=序列化于GII-04; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-19: 全局预扫描兼容既有全局标志与 `--` 透传（重复与交错 `-C`；#686，自 GII-01 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 `ADR-GII-01` ①④，让全局预扫描识别全部既有全局标志（无值、可选值、必选值）并跳过其值 token，使 `-C` 可重复（按出现顺序累积切换）并可出现在既有全局标志之后（解除 `GII-01` 发布中「本版只支持 argv 首位的单个 `-C`」的限制）；同时让 `OutputConfig::resolve_from_argv` 在子命令 `--` 处停止扫描，不把透传参数当作输出标志。拆分原因（G-09）：原 `GII-01` 按独立谓词计数超过 G-03 上限；本卡与 `GII-01` 分属「`-C` 目录切换」与「既有全局标志交错 / `--` 透传」两个可独立恢复的轴，`GII-01` 单独发布时在其声明的范围内语义完整（非首位的 `-C` 按现状拒绝、有文档说明并由其 AC3 验证），本卡追加于 Phase 0 末尾。

**Out of scope:** `-C` 本身的目录切换、路径值不触发输出标志与错误语义（`GII-01`）；`--git-dir` / `--work-tree` 路径值的跳过（`GII-02`、`GII-24`）；别名展开后的输出模式（`GII-05`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 既有全局标志定义：`--json[=<fmt>]`/`-J`（可选值）、`--color[=<when>]`、`--progress <MODE>`（必选值）、`--max-connections <N>`（必选值）及若干无值标志 | `src/cli.rs:234-336` |
| `OutputConfig::resolve_from_argv` 扫描整个 argv，不在 `--` 处停止 | `src/utils/output.rs:250-327` |

**Acceptance criteria:**

- [ ] AC1：`libra --json -C <dir> status` 输出 `<dir>` 状态的 JSON。
- [ ] AC2：无值、`=` 形式可选值与必选值的既有全局标志同时位于 `-C` 之前时（`libra --no-pager --color=never --max-connections 8 -C <dir> status`）在 `<dir>` 内执行。
- [ ] AC3：`libra --progress text -C <dir> status` 在 `<dir>` 内执行。
- [ ] AC4：`libra --progress=text -C <dir> status` 在 `<dir>` 内执行。
- [ ] AC5：`--progress` 缺值（`libra --progress -C <dir> status`）以 `LBR-CLI-002` 失败。
- [ ] AC6：`libra status -- --json` 失败时以人读格式输出错误（`--` 之后的参数不触发 JSON 模式）。
- [ ] AC7：重复的 `libra -C <d1> -C <d2> status` 在 `<d1>/<d2>` 内执行。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`、`COMPATIBILITY.md` 及 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`），按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::init_test::global_dash_c_interleaves_with_global_flags` 承载；夹具：`<tmp>/r` 已提交一次且含未跟踪 `u.txt`，`<tmp>/outside` 无仓库，基线 `B` = 在 `<tmp>/r` 内运行 `libra status --short` 的 stdout；AC1–AC5 的 cwd 为 `<tmp>/outside`）

- AC1：退出 0，stdout 可被解析为 JSON，其未跟踪文件列表含 `u.txt`。
- AC2：`libra --no-pager --color=never --max-connections 8 -C <tmp>/r status --short` 退出 0，stdout 等于 `B`（`--color` 与 `--json` 为 `require_equals` 的可选值标志、`--max-connections` 为必选值标志，`src/cli.rs:234-336`；未跳过其中任一值 token 的实现会把它当作子命令而失败）。
- AC3：`libra --progress text -C <tmp>/r status --short` 退出 0，stdout 等于 `B`。
- AC4：`libra --progress=text -C <tmp>/r status --short` 退出 0，stdout 等于 `B`。
- AC5：`libra --progress -C <tmp>/r status` 的 stderr 含 `Error-Code: LBR-CLI-002`。
- AC6：cwd 为无仓库的 `<tmp>/outside`，`libra status -- --json` 的 stderr 含人读错误标记 `Error-Code: LBR-REPO-001`（成功路径中 clap 已把 `--` 后的 `--json` 当作 pathspec；错误路径在 `--` 之后仍扫描原始 argv 的错误实现会输出 JSON 错误信封，`src/main.rs:699-703`）。
- AC7：cwd 为 `<tmp>/outside`，`libra -C <tmp> -C r status --short` 退出 0，stdout 等于 `B`（只消费首个 `-C` 的实现会以 `LBR-CLI-002` 失败）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::global_dash_c_interleaves_with_global_flags)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 触及 src/cli.rs 全局 flag 预扫描及版本面集合` | **Dependencies:** `GII-01, DEP-GII-03`

**Implementation write set:** `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `tests/command/init_test.rs`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `tests/command/init_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/README.md`、`docs/commands/zh-CN/README.md`（全局标志章节删除「本版只支持 argv 首位的单个 `-C`」的限制、说明 `-C` 可重复且按出现顺序累积，并说明 `--` 之后参数不影响输出模式）、`COMPATIBILITY.md`（原地更新既有行，不改行数）与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`（`## Global Flags`）；`docs/development/commands/README.md` 无全局标志章节，`N/A`。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=全局预扫描:重复-C、与既有标志交错及--透传; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=GII-01,DEP-GII-03; writeset=序列化于GII-01; release=independent; split-from=GII-01; exception=N/A`

---

### Task GII-06: `init --template` 缺失模板目录降级为告警并继续建库（#942）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：缺失/不可读 `--template` 打印 Git 告警并按空模板布局初始化；超长路径同路径；self-reference guard 不变；新增 AC1–AC3/AC5–AC7 用例。D 组 patch +1。偏差：用户已取消独立审计与全量测试。）

**Description:** `--template` 指向不存在（含 `ENAMETOOLONG` 等无法 `stat`）的目录时，不再以致命错误中止，而是向 stderr 打印 `warning: templates not found in <path>` 并按「无模板」继续创建仓库（匹配 Git，上游 t0001#22 TC-1692）。

**Out of scope:** 自引用 / 仓库内递归模板安全守卫（`src/command/init.rs:737-775`）保持 fail-closed 不变；`GIT_TEMPLATE_DIR` / `init.templateDir` 来源指向缺失目录的处理（尚未排期，重启条件：出现对应上游用例或 issue）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `InitError` 定义 `template directory '{path}' does not exist`，`--template` 解析后在建库前报错；已安装 0.30.207 实测 `libra init --template=/nonexistent t2` → `fatal: template directory '/nonexistent' does not exist`、`LBR-IO-001`、退出 128 | `src/command/init.rs:105,197-200,722-726,1634`、#942 |
| 空值 `--template=` 表示「无模板」 | `src/command/init.rs:720-726` |

**Acceptance criteria:**

- [ ] AC1：`--template` 指向缺失目录时 `libra init --template=<tmp>/missing <dir>` 继续建库，且仓库布局与「无模板」初始化相同。
- [ ] AC2：AC1 的 stderr 含 `warning: templates not found in <tmp>/missing`。
- [ ] AC3：10,001 字符的超长模板路径（`stat` 返回 `ENAMETOOLONG`）同样继续建库，且仓库布局与「无模板」初始化相同。
- [ ] AC4：自引用模板（`--template` 等于目标仓库存储目录）仍以 `LBR-CLI-002` 被拒绝。
- [ ] AC5：AC3 的 stderr 含 `warning: templates not found in <该超长路径>`。
- [ ] AC6：AC1 建成的仓库可正常打开。
- [ ] AC7：AC3 建成的仓库可正常打开。
- [x] ER-06/ER-06a：`init` 中英/开发文档、COMPATIBILITY 与网站已同步缺失模板告警。

**AC → 断言映射:**（AC1–AC3、AC5–AC7 由 `(new)` `command::init_test::missing_template_warns_and_succeeds` 承载；AC4 由既有 `command::init_test::test_init_template_self_reference_guard` 承载）

- AC1：`libra init --template=<tmp>/missing <dir>` 须退出 0（执行条件）；`<dir>/.libra` 与 `libra init --template= <dir2>` 生成的 `<dir2>/.libra` 的递归相对路径集合（全部文件与目录，不比较内容）相同。空模板布局为 `libra.db`、`objects/info`、`objects/pack`、`vault.db`，默认模板另有 `hooks/pre-commit.sh`、`hooks/pre-commit.ps1`、`info/exclude`（已安装 0.30.207 实测，`src/command/init.rs:720-721`），错误地装入默认 hooks 或 `info/exclude` 的实现不满足。
- AC2：AC1 的 stderr 含 `warning: templates not found in <tmp>/missing`。
- AC3：模板路径为 `<tmp>/` + 10,001 个 `x`，`libra init` 须退出 0（执行条件）；`<dir3>/.libra` 的递归相对路径集合与 AC1 的 `<dir2>/.libra` 相同。
- AC5：AC3 那次 `libra init` 的 stderr 含 `warning: templates not found in ` 后接该完整超长路径（只对普通缺失路径告警、对 `ENAMETOOLONG` 静默的错误实现不满足）。
- AC6：`libra -C <dir> status` 退出 0（建库后无法打开的错误实现不满足）。
- AC7：`libra -C <dir3> status` 退出 0。
- AC4：既有用例保持通过且本卡不改其断言：在已有仓库内 `libra init --template=.libra` 的 `error_code` 为 `LBR-CLI-002`（`tests/command/init_test.rs:1234-1243`；该用例的其余既有断言不作本 AC 判据）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步。

**Verification:**

- [x] 卡特有用例：`missing_template_warns_and_succeeds`（通过）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::init_test::.*template/)'`（2026-11-18 核对现有 12 条；按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-05, DEP-GII-03`

**Implementation write set:** `src/command/init.rs`, `tests/command/init_test.rs`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/init.rs`, `tests/command/init_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md`（原地更新 `init` 行，不改行数）；网站 `init.en.md` 由 `sync-libra-command-docs.py --only init` 生成。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=init_--template缺失目录告警; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=GII-05,DEP-GII-03; writeset=序列化于GII-05; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-07: 核实 `init --shared=0666` 已符合 Git 并收口 #943（audit）

**Task type:** `audit` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 GC-01 核对，#943 报告的拒绝（0.30.91：`invalid shared mode '0666': numeric modes must keep repository directories traversable`）在当前源码与已安装 0.30.207 中已不复现。本卡以只读复现核实 #943 验收 1–5，并在命令文档与 `COMPATIBILITY.md` 补 `0666` 示例后收口 #943；按 B 组 `audit` 类型门保持 no-code / no-config（不改生产代码与测试）。若复现结果与下列 AC 不符，先按 ER-03 把本卡改为 `implementation` 卡（同步 `Release boundary`、`Version increment`、`Release write set`）再执行。

**Out of scope:** `0666` 的自动化回归用例（尚未排期；重启条件：下一张触及 `src/utils/shared_perm.rs` 的实现卡把该用例并入其同轴验收）；#943 验收 6（上游 `t0001-init.sh` #20 读取 Git 兼容的 `<dir>/config` 视图，issue 声明另行跟踪）；setgid 位（依赖运行用户与目录所属组的关系）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `SharedMode::parse` 对数字模式只拒绝缺 owner 读写（`OwnerPermissionsMissing`），其余按 `& 0o666` 保存 | `src/utils/shared_perm.rs:32-60` |
| `calc_shared_perm` 对目录按 `(mode & 0o444) >> 2` 补执行位，组可写时加 setgid | `src/utils/shared_perm.rs:205-237` |
| 2026-11-18 以已安装 0.30.207 运行下方复现脚本：`bare_init_rc 0`、`bare_cfg 0666`、`bare_objects 0o777`、`bare_db 0o666`；`plain_init_rc 0`、`plain_cfg 0666`、`plain_objects 0o777`、`plain_db 0o666`；`mode0770_rc 0`；`mode0400_rc 129`、`LBR-CLI-002`、未建库 | 本卡 `Verification` 第 1 项 |
| 文档已描述掩码与目录执行位，但无 `0666` 示例 | `docs/commands/init.md:240-250`、`COMPATIBILITY.md:165` |

**Acceptance criteria:**

- [ ] AC1：`libra init --bare --shared=0666 b.git` 退出 0。
- [ ] AC2：该裸仓库存储位于 `b.git` 本身（`b.git/libra.db` 存在）。
- [ ] AC3：该裸仓库 `core.sharedRepository` 读出为 `0666`。
- [ ] AC4：`b.git/objects` 的权限含 `0o111` 执行位。
- [ ] AC5：`b.git/libra.db` 的权限为 `0o666`。
- [ ] AC6：非裸 `libra init --shared=0666 p` 退出 0。
- [ ] AC7：该非裸仓库 `core.sharedRepository` 读出为 `0666`。
- [ ] AC8：`p/.libra/objects` 的权限含 `0o111` 执行位。
- [ ] AC9：`p/.libra/libra.db` 的权限为 `0o666`。
- [ ] AC10：`libra init --shared=0770 q` 退出 0（#943 验收 5）。
- [ ] AC11：`libra init --shared=0400 n` 以 `LBR-CLI-002` 失败。
- [ ] AC12：AC11 的失败不创建 `n/.libra`。
- [ ] AC13：`docs/commands/init.md` 的 `--shared` 小节含 `--shared=0666` 示例。
- [ ] AC14：AC13 的示例所述目录与数据库权限结果与 AC4、AC5、AC8、AC9 实测一致。
- [ ] AC15：`docs/commands/zh-CN/init.md` 的 `--shared` 小节含同一 `--shared=0666` 示例。
- [ ] AC16：AC15 的示例所述结果与 AC4、AC5、AC8、AC9 实测一致。
- [ ] AC17：`docs/development/commands/init.md` 的 `--shared` 说明含同一 `--shared=0666` 示例。
- [ ] AC18：AC17 的示例所述结果与 AC4、AC5、AC8、AC9 实测一致。
- [ ] AC19：`COMPATIBILITY.md` 的 `init` 行八进制模式示例列表含 `0666`（原地改写；行数不变由「文档与网站验收命令」第 1 步核对，属 ER-06a 强制门、不计入）。
- [ ] AC20：网站 `init.en.md` 含同一 `--shared=0666` 示例。
- [ ] ER-06/ER-06a 网站交付流程（不计入 G-03；本卡特有的文档语义判据已由 AC13–AC20 计数）：按「文档与网站验收命令」第 1–7 步执行。

**AC → 断言映射:**（AC1–AC12 由 `Verification` 第 1 项的复现脚本输出逐项判定：`<B>` 为执行当日本仓库最终树 `cargo build --release` 产物，复制为仓库外临时目录中的 `libra`；脚本在临时目录内以隔离 `HOME` / `LIBRA_CONFIG_GLOBAL_DB` 运行，输出原样写入本卡）

- AC1：`bare_init_rc 0`。
- AC2：`bare_layout True`。
- AC3：`bare_cfg 0666`。
- AC4：`bare_objects` 的八进制值与 `0o111` 按位与等于 `0o111`。
- AC5：`bare_db 0o666`。
- AC6：`plain_init_rc 0`。
- AC7：`plain_cfg 0666`。
- AC8：`plain_objects` 与 `0o111` 按位与等于 `0o111`。
- AC9：`plain_db 0o666`。
- AC10：`mode0770_rc 0`。
- AC11：`mode0400_rc` 非 0（执行条件）且 `mode0400_code True`（数值退出码本身不作判据）。
- AC12：`mode0400_no_repo True`。
- AC13 → `Verification` 第 3 项。
- AC14 → `Verification` 第 4 项。
- AC15 → `Verification` 第 5 项。
- AC16 → `Verification` 第 6 项。
- AC17 → `Verification` 第 7 项。
- AC18 → `Verification` 第 8 项。
- AC19 → `Verification` 第 9 项。
- AC20 → `Verification` 第 10 项。
- 网站交付流程（不计入 G-03）→ 「文档与网站验收命令」。

**Verification:**

- [ ] 手工证据（AC1–AC12）：`LIBRA_AUDIT_BIN=<临时目录>/libra python3 -I -c "import os,tempfile,pathlib,subprocess,stat; b=os.environ['LIBRA_AUDIT_BIN']; d=pathlib.Path(tempfile.mkdtemp()); h=d/'home'; h.mkdir(); env=dict(os.environ,HOME=str(h),LIBRA_CONFIG_GLOBAL_DB=str(h/'global.db')); r=lambda a,c=d: subprocess.run([b,*a],cwd=c,env=env,capture_output=True,text=True); m=lambda p: oct(stat.S_IMODE(os.stat(p).st_mode)); x=r(['init','--bare','--shared=0666','b.git']); print('bare_init_rc',x.returncode,'bare_layout',(d/'b.git'/'libra.db').exists(),'bare_cfg',r(['config','get','core.sharedRepository'],d/'b.git').stdout.strip(),'bare_objects',m(d/'b.git'/'objects'),'bare_db',m(d/'b.git'/'libra.db')); y=r(['init','--shared=0666','p']); print('plain_init_rc',y.returncode,'plain_layout',(d/'p'/'.libra'/'libra.db').exists(),'plain_cfg',r(['config','get','core.sharedRepository'],d/'p').stdout.strip(),'plain_objects',m(d/'p'/'.libra'/'objects'),'plain_db',m(d/'p'/'.libra'/'libra.db')); z=r(['init','--shared=0770','q']); print('mode0770_rc',z.returncode,'mode0770_cfg',r(['config','get','core.sharedRepository'],d/'q').stdout.strip()); w=r(['init','--shared=0400','n']); print('mode0400_rc',w.returncode,'mode0400_code','Error-Code: LBR-CLI-002' in w.stderr,'mode0400_no_repo',not (d/'n'/'.libra').exists())"`（输出原样写入本卡）
- [ ] B 组结构与链接门（ER-04 B 组 `audit` 类型强制门，不计入 G-03 VER）：本卡产物文件均存在且 `--shared` 章节完整；产物中本卡改动处的内部链接与 `file:line` 锚点可解析；`docs/commands/init.md` 与 `docs/commands/zh-CN/init.md` 双份存在；以开工前记录的 `libra status --short` 基线（各改动路径及其 SHA-256）为准，收尾时新出现的改动路径与 SHA-256 发生变化的基线路径都必须属于 `Deliverables` 中的本仓库文件或协调文件（本计划文件、`docs/development/plan/plan-status.md`），出现其它路径即 STOP 并核查归属；基线中的无关改动保持原样，不得还原、暂存或提交（GC-12）
- [ ] 文档存在性核对（AC13，手工证据）：`rg -n -F -- '--shared=0666' docs/commands/init.md` 退出 0（退出 1 即本 AC 失败，>1 判执行失败），人工确认命中行位于 `--shared` 小节；记录命中行号与文件 SHA-256。
- [ ] 文档语义核对（AC14，手工证据）：人工确认第 3 项定位的示例所述目录执行位与数据库权限结果与第 1 项输出（`bare_objects`、`bare_db`、`plain_objects`、`plain_db`）一致；记录结论。
- [ ] 文档存在性核对（AC15，手工证据）：同第 3 项，对 `docs/commands/zh-CN/init.md` 执行。
- [ ] 文档语义核对（AC16，手工证据）：同第 4 项，对第 5 项定位的示例执行。
- [ ] 文档存在性核对（AC17，手工证据）：同第 3 项，对 `docs/development/commands/init.md` 执行。
- [ ] 文档语义核对（AC18，手工证据）：同第 4 项，对第 7 项定位的示例执行。
- [ ] 文档存在性核对（AC19，手工证据）：以 `rg -n -F 'e.g. 0660' COMPATIBILITY.md` 定位 `init` 行（2026-11-18 为第 165 行）的八进制示例列表，人工确认列表含 `0666`（如 `(e.g. 0660, 0640, 0600, 0664, 0666, 0775)`）；记录文件 SHA-256。
- [ ] 文档存在性核对（AC20，手工证据）：「文档与网站验收命令」第 3 步之后，`rg -n -F -- '--shared=0666' /Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md` 退出 0（退出 1 即本 AC 失败，>1 判执行失败）；记录文件 SHA-256。

**Full-suite trigger:** `N/A（no-release，不执行 C 组；仓库改动以独立本地提交随 GII-08 的 C 组 PR 推送，见「实施顺序」承载例外）` | **Dependencies:** `GII-06, DEP-GII-03`

**Deliverables:** `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`

**Implementation write set:** `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `N/A`（no-release） | **Files likely touched:** `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件在 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md` 的 `--shared` 小节补 `0666`（目录补执行位，裸 / 非裸一致）示例，`COMPATIBILITY.md` 原地在 `init` 行的数字模式示例中补 `0666`（不改行数）；网站 `init.en.md` 由 `sync-libra-command-docs.py --only init` 生成。按「文档与网站验收命令」完成全部步骤。

**补偿动作（`compensating`）：** 本卡的外部副作用是推送到 `cf` 的网站提交；本仓库提交随承载卡 `GII-08` 的 PR 进入 `main`。修正或撤回 `0666` 示例一律以新提交前滚，不改写、不回退已有提交，不强推（不使用 `libra revert`：它以 `HEAD` 为基础重建 index 并改写已跟踪工作区文件，可能覆盖无关改动，`src/command/revert.rs:1370,1618,1820`）。**补偿命令：** 人工编辑 `Deliverables` 中受影响的文件；本仓库以 `libra add <本次修改的文件>` 与 `libra commit -s -m "docs(init): ..."` 形成新提交，并按「实施顺序」承载例外核对暂存与提交文件集合；仓库文档与网站按「文档与网站验收命令」第 1–7 步验收、交付并核验部署（纯删除改动按第 7 步新增说明句作为 `M`）。**归属（按 ER-10，不另设流程）：** ① `GII-08` 开工前（`Lifecycle=pending`）发现——属本卡行为轴内的就地修复：本卡 `Acceptance` 置空（`Lifecycle` 保持 `in-progress`），修正后的文档不再满足 AC13–AC20 时先按 ER-03 修订这些 AC 及对应 `Verification` 与 `Granularity`，再重跑本卡全部 `Verification` 与 ER-05，重新满足承载例外的前驱条件后 `GII-08` 方可开工；② `GII-08` 开工后发现——按 ER-10 新建 `FIX-*`，并以 ER-03 计划修订登记其 `Task type`、写集、无环依赖边与具名 C/D 承载点（经计划文本双审后执行）；本卡已记录的验收结果保持事实原状、不回退（其 AC13–AC20 所述示例此后以该 `FIX-*` 的验收为准），与本计划其它卡交付后发现缺陷时按 ER-10 前滚的口径一致。**幂等键：** ① 为本卡 `Verification` 中的修正记录，② 为该 `FIX-*` 卡 ID，以及各自已记录的提交 SHA；中断后按第 2 步「续作例外」续作，已存在的提交不重复创建，提交后的签名核验、推送、远端核对与部署核验不得因已有提交而跳过。补偿完成后两个仓库的 `0666` 示例一致；`0666` 行为本身未被本卡改动，不受影响。

**Rollback mode:** `compensating` | **Estimated scope:** `S` | **Version increment:** `N/A`（随 `GII-08` 的发布进入用户渠道） | **Release boundary:** `no-release` | **C/D coverage from:** `GII-08`

**Granularity:** `type=audit; axis=核实init_--shared=0666并收口#943; recovery=compensating; complete=yes; self-contained=yes; AC=20/20; VER=9/20; landing=0; prod-files=0; scope=S; deps=GII-06,DEP-GII-03; writeset=序列化于GII-06; release=no-release; split-from=N/A; exception=N/A`

---

### Task GII-08: 统一 `init` 重初始化告警文案 `ignored --initial-branch`（#946）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：重初始化告警改为 Git 文案 `re-init: ignored --initial-branch=<name>`；新增/更新测试；C 组 patch +1。偏差：init 共享过滤 20 条中 4 条旧 SQLite fixture 因默认 redb 失败，超出本卡；用户已取消独立 Claude/Codex 审计与全量测试。）

**Description:** 将 `src/command/init.rs:1426` 重初始化时输出的 `ignoring --initial-branch '<name>' on re-initialization; keeping '<branch>'` 改为与 Git 一致的 `re-init: ignored --initial-branch=<name>`（上游 t0001#90 TC-1702 断言子串 `ignored --initial-branch`）。本卡的 C 组 PR 同时承载 `GII-07` 的独立本地提交（`GII-07` 为 `no-release` 卡，C/D 继承本卡；见「实施顺序」承载例外）。

**Out of scope:** 不改变重初始化保持既有默认分支的行为。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `init.rs` 输出 `"ignoring --initial-branch '{requested}' on re-initialization; keeping '{initial_branch}'"`；已安装 0.30.207 实测重初始化 stderr 为 `warning: ignoring --initial-branch 'foo' on re-initialization; keeping 'main'` | `src/command/init.rs:1426`、#946 |

**Acceptance criteria:**

- [ ] AC1：`libra init -b foo` 重初始化已有仓库时 stderr 含完整一行 Git 文案 `warning: re-init: ignored --initial-branch=foo`。
- [ ] AC2：重初始化后 `HEAD` 仍指向原分支（`refs/heads/main`），不切换到 `foo`。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件核对并同步 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md` 与网站 `init.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::init_test::reinit_ignored_initial_branch_warning` 承载）

- AC1：先 `libra init -b main <r>`（显式固定初始分支，不受 `init.defaultBranch` 配置影响，`src/command/init.rs:609-617`），再 `libra init -b foo <r>`：第二次须退出 0（执行条件），stderr 含完整一行 `warning: re-init: ignored --initial-branch=foo`（只含子串 `ignored --initial-branch` 的其它措辞不满足）。
- AC2：`libra -C <r> symbolic-ref HEAD` 输出 `refs/heads/main`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步。

**Verification:**

- [x] 卡特有用例：`reinit_ignored_initial_branch_warning`（通过）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::init_test::.*(initial_branch|reinit)/)'`（2026-11-18 核对现有 19 条；按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-07, DEP-GII-03`（`GII-07` 达 `Acceptance=locally-accepted`、ER-05 review `PASS` 且已形成独立本地提交即满足，见「实施顺序」承载例外）

**Implementation write set:** `src/command/init.rs`, `tests/command/init_test.rs`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/init.rs`, `tests/command/init_test.rs`

**Docs and compatibility impact:** 逐文件核对 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md` 中重初始化告警文案（含旧文案时同步改写，不改 `COMPATIBILITY.md` 行数；无旧文案的文件在验收记录中写明「核对无需修改」及其 SHA-256）；网站 `init.en.md` 由 `sync-libra-command-docs.py --only init` 生成或经 `--check` 确认无漂移。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=init重初始化--initial-branch告警文案; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=GII-07,DEP-GII-03; writeset=序列化于GII-07; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-09: 补全 `repositoryformatversion` 与扩展存在性校验（#948）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在仓库命令预检（`src/cli.rs:3681-3698`：需要仓库的命令在此调用 `set_local_hash_kind_for_storage`（`src/cli.rs:112`）读取 `core.objectformat` 设定哈希算法）中新增仓库格式校验（同一校验函数同时接入无 schema 守卫的预检路径 `set_local_hash_kind_for_storage_without_schema_guard`，`src/cli.rs:145`，调用点 3694；`hash-object`、`verify-pack`、`count-objects` 及部分 `worktree` 子命令经该路径，`src/cli.rs:1810-1906`），按 Git 规则：`core.repositoryformatversion` 大于 1 拒绝；版本 0 出现 v1-only 扩展 `extensions.objectformat` 拒绝；版本 1 出现 `objectformat` 以外的 `extensions.<name>` 拒绝（本卡只把 `extensions.objectformat` 视为已支持扩展，其取值与一致性校验由 `GII-22` 承接）。既有仅含 `core.repositoryformatversion=0` 与 `core.objectformat` 的 Libra 仓库不受影响。**修复通道：** 本地 `libra config` 当前同样经过该预检（`src/cli.rs:1889` 只让 `--global` / `--system` / schema doctor 免预检，其余在 `src/cli.rs:3687` 调用 `set_local_hash_kind_for_storage`），因此本卡须显式让 `Commands::Config` 跳过格式校验（仍按 `core.objectformat` 设定哈希算法），使本卡拒绝的每个状态都能用 `libra config set` / `libra config unset` 修复——这是与 Git 的有意差异（Libra 配置存于 SQLite，无法像 `.git/config` 那样手工编辑），按 GC-03 写入 `init` 与 `config` 文档及 `COMPATIBILITY.md`。拆分说明（G-09）：本卡于 2026-11-18 拆出 `GII-22`（`extensions.objectformat` 取值与一致性）与 `GII-20`（`extensions.refstorage` 规则），二者追加于 Phase 1 末尾、链上依次接在本卡之后；原因是修复通道按每个非法状态各计一条 AC 后超过 G-03 上限。

**Out of scope:** `extensions.objectformat` 的取值校验与 `core.objectformat` 一致性校验（`GII-22`；在本卡发布中，版本 1 下任意取值的 `extensions.objectformat` 仍被接受，与当前行为相同）；`extensions.refstorage` 的专门规则（`GII-20`；在本卡发布中，版本 1 下的 `extensions.refstorage` 与其它未支持扩展一样被拒绝，版本 0 下按 Git 对未知扩展的处理被忽略）；`rev-parse --show-object-format`（#947，不在本计划跟踪范围，尚未排期）；把 SHA-256 仓库改写为 `repositoryformatversion=1` + `extensions.objectformat` 的布局迁移（尚未排期）；SQLite schema 变更（无）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 新库写入 `repositoryformatversion=0` 与 `core.objectformat`，不写 `extensions.*` | `src/command/init.rs:2373-2378,2399` |
| 仓库命令预检只读取 `core.objectformat` 设定哈希算法，不校验 `repositoryformatversion` / `extensions.*` | `src/cli.rs:95-170,3681-3698` |
| 本地 `libra config` 经同一预检（仅 `--global` / `--system` / schema doctor 免预检） | `src/cli.rs:1889,3687` |
| 重初始化只读校验已存的 `core.objectformat`（未知值拒绝）；`extensions.objectformat` 需 `repositoryformatversion=1` 的规则只存在于 Git 源转换路径 | `src/command/init.rs:1327-1413`（重初始化）、`src/command/init.rs:1816-1833`（Git 源转换） |
| #948：0.30.91 接受 `libra config extensions.objectformat garbage` 后 `status` 仍 rc=0 | #948 |

**Acceptance criteria:**

- [ ] AC1：`core.repositoryformatversion` 大于 1 时仓库命令以 `LBR-REPO-002` 失败。
- [ ] AC2：版本 0 且存在 `extensions.objectformat` 时仓库命令的 stderr 含 `repo version is 0, but v1-only extension found`（命令以非 0 退出为执行条件）。
- [ ] AC3：版本 1 且存在未支持的 `extensions.<name>` 时以 `LBR-REPO-002` 失败。
- [ ] AC4：版本 1 且 `extensions.objectformat` 与 `core.objectformat` 一致（`sha256`）时仓库命令正常（退出 0）。
- [ ] AC5：AC1 的状态经 `libra config set core.repositoryformatversion 0` 修复后，`libra status` 退出 0。
- [ ] AC6：AC2 的状态经 `libra config unset extensions.objectformat` 修复后，`libra status` 退出 0。
- [ ] AC7：AC3 的状态经 `libra config unset extensions.nosuchext` 修复后，`libra status` 退出 0。
- [ ] AC8：版本大于 1 时，经无 schema 守卫预检的 `libra count-objects` 同样以 `LBR-REPO-002` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/{init,config}.md`、`docs/commands/zh-CN/{init,config}.md`、`docs/development/commands/{init,config}.md`、`COMPATIBILITY.md` 与网站 `init.en.md`、`config.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::init_test::repositoryformatversion_and_extensions_validation` 承载；每个非法场景使用新建的独立仓库，用 `libra config set` 写入对应键后运行 `libra status`；修复 AC 中的修复命令须退出 0，否则该 AC 失败）

- AC1：`core.repositoryformatversion=2` → stderr 含 `Error-Code: LBR-REPO-002`。
- AC2：版本 0 + `extensions.objectformat=sha1` → 须以非 0 退出（执行条件），stderr 含 `repo version is 0, but v1-only extension found`。
- AC3：版本 1 + `extensions.nosuchext=true` → stderr 含 `Error-Code: LBR-REPO-002`。
- AC4：`libra init --object-format=sha256 <s>` 后写入版本 1 + `extensions.objectformat=sha256` → `libra status` 退出 0。
- AC5：在 AC1 的仓库上 `libra config set core.repositoryformatversion 0` 退出 0 → `libra status` 退出 0。
- AC6：在 AC2 的仓库上 `libra config unset extensions.objectformat` 退出 0 → `libra status` 退出 0。
- AC7：在 AC3 的仓库上 `libra config unset extensions.nosuchext` 退出 0 → `libra status` 退出 0。
- AC8：另一新建仓库写入 `core.repositoryformatversion=2` → `libra count-objects`（`src/cli.rs:1823-1830`）的 stderr 含 `Error-Code: LBR-REPO-002`（只改普通预检路径的错误实现会让该命令成功）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::repositoryformatversion_and_extensions_validation)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::init_test::.*object_format/) or test(/^command::config_test::/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 触及 src/cli.rs 仓库打开预检与共享 config 校验及版本面集合` | **Dependencies:** `GII-08, DEP-GII-03`

**Implementation write set:** `src/cli.rs`, `src/command/init.rs`, `src/internal/config.rs`, `tests/command/init_test.rs`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/command/init.rs`, `src/internal/config.rs`, `tests/command/init_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`（格式版本与扩展存在性规则）、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`（非法格式状态下 `libra config` 仍可用于修复的有意差异）、`COMPATIBILITY.md`（原地更新 `init` / `config` 行，不改行数）；网站 `init.en.md`、`config.en.md` 由 `sync-libra-command-docs.py --only init config` 生成。错误码复用既有 `LBR-REPO-002`，`docs/error-codes.md` 不修改（`N/A`）。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=repositoryformatversion与扩展存在性校验; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=3; prod-files=3; scope=M; deps=GII-08,DEP-GII-03; writeset=序列化于GII-08; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-20: `extensions.refstorage` 校验（#948，自 GII-09 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `GII-09` 建立的仓库格式校验中加入 `extensions.refstorage` 的专门规则：它是 v1-only 扩展（版本 0 出现即拒绝）；版本 1 下接受 `files`（Libra 引用存于 SQLite，`files` 作为 Git 兼容声明接受），`reftable` 以不支持拒绝，其它取值按非法值拒绝；`GII-09` 的修复通道（`libra config` 跳过格式校验）对本卡新增的被拒状态同样适用。拆分原因（G-09）：原 `GII-09` 按独立谓词计数超过 G-03 上限，`extensions.refstorage` 是可独立恢复的规则集，本卡追加于 Phase 1 末尾。

**Out of scope:** 版本号、扩展存在性与 `extensions.objectformat` 规则（`GII-09`、`GII-22`）；Libra 的引用存储实现本身（永久非目标，Libra 不提供 files/reftable 引用后端）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `GII-09` 交付后，版本 1 下的 `extensions.refstorage` 按未支持扩展被拒绝、版本 0 下被忽略 | 本计划 `GII-09` |
| #948 期望：`core.repositoryformatversion = 0` + `extensions.refStorage` 以 `repo version is 0, but v1-only extension found` 失败；非法值以 `invalid value for 'extensions.refstorage'` 失败 | #948 |

**Acceptance criteria:**

- [ ] AC1：版本 0 且存在 `extensions.refstorage` 时仓库命令的 stderr 含 `repo version is 0, but v1-only extension found`（命令以非 0 退出为执行条件）。
- [ ] AC2：版本 1 且 `extensions.refstorage=files` 时仓库命令正常（退出 0）。
- [ ] AC3：版本 1 且 `extensions.refstorage=reftable` 时以 `LBR-REPO-002` 失败。
- [ ] AC4：版本 1 且 `extensions.refstorage` 取 `files` / `reftable` 以外的值时仓库命令的 stderr 含 `invalid value for 'extensions.refstorage'`（命令以非 0 退出为执行条件）。
- [ ] AC5：AC1 的状态经 `libra config unset extensions.refstorage` 修复后，`libra status` 退出 0。
- [ ] AC6：AC3 的状态经 `libra config unset extensions.refstorage` 修复后，`libra status` 退出 0。
- [ ] AC7：AC4 的状态经 `libra config set extensions.refstorage files` 修复后，`libra status` 退出 0。
- [ ] AC8：版本 1 且 `extensions.refstorage=reftable` 时，经无 schema 守卫预检的 `libra count-objects` 同样以 `LBR-REPO-002` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md` 与网站 `init.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::init_test::refstorage_extension_validation` 承载；每个场景使用新建的独立仓库，用 `libra config set` 写入对应键后运行 `libra status`；修复 AC 中的修复命令须退出 0，否则该 AC 失败）

- AC1：版本 0 + `extensions.refstorage=files` → 须以非 0 退出（执行条件），stderr 含 `repo version is 0, but v1-only extension found`。
- AC2：版本 1 + `extensions.refstorage=files` → `libra status` 退出 0。
- AC3：版本 1 + `extensions.refstorage=reftable` → stderr 含 `Error-Code: LBR-REPO-002`。
- AC4：版本 1 + `extensions.refstorage=garbage` → 须以非 0 退出（执行条件），stderr 含 `invalid value for 'extensions.refstorage'`。
- AC5：在 AC1 的仓库上 `libra config unset extensions.refstorage` 退出 0 → `libra status` 退出 0。
- AC6：在 AC3 的仓库上 `libra config unset extensions.refstorage` 退出 0 → `libra status` 退出 0。
- AC7：在 AC4 的仓库上 `libra config set extensions.refstorage files` 退出 0 → `libra status` 退出 0。
- AC8：另一新建仓库写入版本 1 + `extensions.refstorage=reftable` → `libra count-objects` 的 stderr 含 `Error-Code: LBR-REPO-002`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::refstorage_extension_validation)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::init_test::.*(object_format|extensions)/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 触及 src/cli.rs 仓库打开预检及版本面集合` | **Dependencies:** `GII-22, DEP-GII-03`

**Implementation write set:** `src/cli.rs`, `src/internal/config.rs`, `tests/command/init_test.rs`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/internal/config.rs`, `tests/command/init_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`（`extensions.refstorage` 取值与 v1-only 规则）、`COMPATIBILITY.md`（原地更新 `init` 行，不改行数）；网站 `init.en.md` 由 `sync-libra-command-docs.py --only init` 生成。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=extensions.refstorage校验; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=GII-22,DEP-GII-03; writeset=序列化于GII-22; release=independent; split-from=GII-09; exception=N/A`

---

### Task GII-22: `extensions.objectformat` 取值与一致性校验（#948，自 GII-09 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `GII-09` 建立的仓库格式校验中加入 `extensions.objectformat` 的取值规则：版本 1 下其取值须为对象格式名的单一事实源 `parse_config_value`（`src/internal/object_format.rs:20-27`，接受 `sha1` / `sha256` / `blake3`，与 `core.objectformat` 的取值集合一致；`blake3` 为 Libra 扩展取值）所接受的值，否则以 `invalid value for 'extensions.objectformat'` 拒绝；取值合法但与 `core.objectformat` 不一致时拒绝。`GII-09` 的修复通道（`libra config` 跳过格式校验）对本卡新增的被拒状态同样适用。拆分原因（G-09）：原 `GII-09` 的修复通道按每个非法状态各计一条 AC 后超过 G-03 上限，`extensions.objectformat` 的取值规则是可独立恢复的规则集，本卡追加于 Phase 1 末尾，在串行链中位于 `GII-09` 与 `GII-20` 之间。

**Out of scope:** 版本号与扩展存在性规则（`GII-09`）；`extensions.refstorage`（`GII-20`）；对象格式布局迁移（尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `GII-09` 交付后，版本 1 下任意取值的 `extensions.objectformat` 仍被接受，且不与 `core.objectformat` 比较 | 本计划 `GII-09` |
| 对象格式名的单一事实源为 `parse_config_value`（`sha1` / `sha256` / `blake3`） | `src/internal/object_format.rs:20-27` |
| #948 期望：非法值以 `invalid value for 'extensions.objectformat'` 失败 | #948 |

**Acceptance criteria:**

- [ ] AC1：版本 1 且 `extensions.objectformat` 取 `parse_config_value` 不接受的值时仓库命令的 stderr 含 `invalid value for 'extensions.objectformat'`（命令以非 0 退出为执行条件）。
- [ ] AC2：版本 1 且合法的 `extensions.objectformat` 与 `core.objectformat` 不一致时以 `LBR-REPO-002` 失败。
- [ ] AC3：AC1 的状态经 `libra config set extensions.objectformat sha1` 修复后，`libra status` 退出 0。
- [ ] AC4：AC2 的状态经 `libra config set extensions.objectformat sha256` 修复后，`libra status` 退出 0。
- [ ] AC5：版本 1 且合法的 `extensions.objectformat` 与 `core.objectformat` 不一致时，经无 schema 守卫预检的 `libra count-objects` 同样以 `LBR-REPO-002` 失败。
- [ ] AC6：版本 1 且 `extensions.objectformat=blake3` 与 `core.objectformat=blake3` 一致时仓库命令正常（退出 0）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md` 与网站 `init.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::init_test::objectformat_extension_value_validation` 承载；每个场景使用新建的独立仓库，用 `libra config set` 写入对应键后运行 `libra status`；修复 AC 中的修复命令须退出 0，否则该 AC 失败）

- AC1：`libra init --object-format=sha1 <g>` 新建仓库（显式指定，不受 `GIT_DEFAULT_HASH` 与 `init.defaultObjectFormat` 影响，`src/command/init.rs:1715-1750`）写入版本 1 + `extensions.objectformat=garbage` → 须以非 0 退出（执行条件），stderr 含 `invalid value for 'extensions.objectformat'`。
- AC2：`libra init --object-format=sha256 <s>` 后写入版本 1 + `extensions.objectformat=sha1` → stderr 含 `Error-Code: LBR-REPO-002`。
- AC3：在 AC1 的仓库上 `libra config set extensions.objectformat sha1` 退出 0 → `libra status` 退出 0。
- AC4：在 AC2 的仓库上 `libra config set extensions.objectformat sha256` 退出 0 → `libra status` 退出 0。
- AC5：另一 `libra init --object-format=sha256` 新建仓库写入版本 1 + `extensions.objectformat=sha1` → `libra count-objects` 的 stderr 含 `Error-Code: LBR-REPO-002`。
- AC6：`libra init --object-format=blake3 <b>` 后写入版本 1 + `extensions.objectformat=blake3` → `libra status` 退出 0（错误地只接受 `sha1` / `sha256` 的实现会以 `invalid value` 失败）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::objectformat_extension_value_validation)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::init_test::.*(object_format|extensions)/)'`（含 `GII-09` 的 `repositoryformatversion_and_extensions_validation`；按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 触及 src/cli.rs 仓库打开预检及版本面集合` | **Dependencies:** `GII-09, DEP-GII-03`

**Implementation write set:** `src/cli.rs`, `src/internal/config.rs`, `tests/command/init_test.rs`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/cli.rs`, `src/internal/config.rs`, `tests/command/init_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`（`extensions.objectformat` 取值与一致性规则）、`COMPATIBILITY.md`（原地更新 `init` 行，不改行数）；网站 `init.en.md` 由 `sync-libra-command-docs.py --only init` 生成。错误码复用既有 `LBR-REPO-002`，`docs/error-codes.md` 不修改（`N/A`）。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=extensions.objectformat取值与一致性校验; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=GII-09,DEP-GII-03; writeset=序列化于GII-09; release=independent; split-from=GII-09; exception=N/A`

---

### Task GII-10: 实现 `status -v` / `--verbose`（#834）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `StatusArgs` 新增可重复的 `-v` / `--verbose`：`-v` 在长格式输出后附已暂存变更（HEAD↔index）的 unified diff，`-vv` 再附未暂存变更（index↔worktree）的 unified diff；命令保持只读，修复 t4030#18 因 `status -v` 被拒导致 `HEAD` 未恢复的级联失败。

**Out of scope:** 工作区前导符号链接与 `ELOOP`（由 `GII-11`、`GII-21` 承接）；`-v` 与 `--short` / `--porcelain` 组合的行为（保持当前输出，尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `StatusArgs` 未注册 `-v` / `--verbose`；已安装 0.30.207 实测 `libra status -v` → `error: unexpected argument '-v' found`、`LBR-CLI-002` | `src/command/status/input.rs:27-60`、#834 |
| 长格式渲染与 diff 输出路径 | `src/command/status.rs`、`src/command/status/output.rs` |

**Acceptance criteria:**

- [ ] AC1：`libra status -v` 的 stdout 为长格式状态后接已暂存变更（HEAD↔index）的 unified diff，整体与 pinned Git `git status -v` 的同构输出格式一致。
- [ ] AC2：`libra status -v` 不附未暂存变更（index↔worktree）的 diff。
- [ ] AC3：`libra status -vv` 的 stdout 为长格式状态后依次接已暂存与未暂存变更的 unified diff，整体与 pinned Git `git status -vv` 的同构输出格式一致。
- [ ] AC4：长形式 `libra status --verbose` 与 `libra status -v` 输出相同。
- [ ] AC5：`status -v` / `-vv` 不移动 `HEAD`。
- [ ] AC6：`status -v` / `-vv` 不改写 index。
- [ ] AC7：`status -v` / `-vv` 不改动工作区。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`COMPATIBILITY.md` 与网站 `status.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::status_test::verbose_staged_and_unstaged_diff` 承载；仓库位于分支 `main`，含已提交 `a.txt`，暂存其修改 `staged-line`，另在工作区追加未暂存行 `unstaged-line`。期望文本 `E1` / `E2` 的组成：状态部分为同一状态下 `libra status` 的 stdout（本卡不改变长格式状态文本）；diff 部分及其与状态部分之间的分隔、分段标题，取开工时以 pinned Git `git@6de20f6092` 在同构仓库（相同分支名、文件内容与提交，故 blob OID 相同）中实测的 `git status -v` / `git status -vv` 输出的对应部分，逐字节写入本卡并作为测试常量——不由 Libra 自身的 `diff` 输出生成，因此 diff 渲染缺陷不会同时出现在期望与实际中）

- AC1：`libra status -v` 退出 0；stdout 逐字节等于 `E1`（伪造头部、缺 hunk 或位置错误的输出都不相等）。
- AC2：同一输出不含 `+unstaged-line`。
- AC3：`libra status -vv` 退出 0；stdout 逐字节等于 `E2`（以未暂存 diff 取代已暂存 diff 的输出不相等）。
- AC4：`libra status --verbose` 退出 0，stdout 与 AC1 的 stdout 逐字节相同。
- AC5：在 AC1、AC3、AC4 的每次调用之前与之后各取 HEAD 快照（`libra symbolic-ref HEAD` 的 stdout 与 `libra rev-parse HEAD` 的 stdout 拼接成的文本），每次调用前后逐字节相等（同一提交上切换分支的错误实现也会被识别）。
- AC6：在 AC1、AC3、AC4 的每次调用之前与之后各取 `.libra/index` 的 SHA-256，每次调用前后相等。
- AC7：在 AC1、AC3、AC4 的每次调用之前与之后各取工作区快照（排除 `.libra`，以 `lstat` 不跟随符号链接取得的「路径、类型、权限位、普通文件内容 SHA-256 或链接目标」按路径排序的列表），每次调用前后逐字节相等（改写或删除未暂存内容的错误实现会被识别）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::status_test::verbose_staged_and_unstaged_diff)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::status_test::/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-20, DEP-GII-03`

**Implementation write set:** `src/command/status/input.rs`, `src/command/status.rs`, `src/command/status/output.rs`, `tests/command/status_test.rs`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/status/input.rs`, `src/command/status.rs`, `src/command/status/output.rs`, `tests/command/status_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`COMPATIBILITY.md`（原地更新 `status` 行，不改行数）；网站 `status.en.md` 由 `sync-libra-command-docs.py --only status` 生成。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=status_-v/--verbose; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=GII-20,DEP-GII-03; writeset=序列化于GII-20; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-11: `status` 工作区扫描不经前导符号链接（#1014 的 `status` 部分）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 当已跟踪目录被替换为符号链接时（#1014 复现：原跟踪 `a/b/c/d`、`a/b-2/c/d`，随后 `a/b -> b-2`、`a/b-2 -> b`），`status` 检查已跟踪路径 `a/b-2/c/d` 时虽对末级路径使用 `symlink_metadata`，内核仍会解析前导分量 `a/b-2`（符号链接）并触发 `ELOOP`，于是 `status` 以「输出不完整」退出 128。本卡让 `status` 扫描对已跟踪路径先做前导符号链接检测（Git `has_symlink_leading_path` 语义：任一前导分量为符号链接即视该路径在工作区中已删除），与 Git 2.56.0 一致。operation snapshot 的同类扫描（影响 `add`、`commit`、`reset`、`checkout`、`merge`）于 2026-11-18 按 G-09 拆出为 `GII-21`。

**Out of scope:** operation snapshot 工作区扫描（`GII-21`）；`mv` 越过符号链接写出 index 同名冲突（#1039，`GII-14`）；不改变普通有效符号链接的 `120000` mode 记录；#1008（另一 issue，不在本计划）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `status` 对已跟踪路径调用 `workdir.join(file).symlink_metadata()`；检查失败时发出 `cannot inspect '<path>'` 告警，并以「输出不完整」退出 | `src/command/status.rs:745,1342,1366,1513,1537`、`src/command/status/scan.rs:443,600,807` |
| #1014：`libra status --short` 在复现状态下 `warning: cannot inspect 'a/b-2/c/d': io_error`、rc=128；Git 同状态 rc=0 | #1014 |
| 顶层互指链接 `a -> b`、`b -> a`（无被替换的跟踪目录）在已安装 0.30.207 上 `status` 已退出 0，不能复现本缺陷 | 2026-11-18 实测 |

**Acceptance criteria:**

- [ ] AC1：在 #1014 复现状态下 `libra status --short --no-branch` 的输出行集合与 pinned Git 在同构状态下的 `git status --short --no-branch` 相同（前导符号链接下的两条已跟踪路径报告为工作区删除）。
- [ ] AC2：已跟踪目录 `d` 被替换为指向仓库外目录 `<ext>`（含同名同内容的 `f`）的符号链接时，`libra status --short` 把已跟踪的 `d/f` 报告为已删除（经链接读到同内容 `<ext>/f` 的错误实现会把它视为未变更）。
- [ ] AC3：已跟踪目录 `d` 被替换为指向不存在目标的符号链接时，`libra status --short` 把已跟踪的 `d/f` 报告为已删除。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`COMPATIBILITY.md` 与网站 `status.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::status_test::status_does_not_traverse_leading_symlinks` 承载，`#[cfg(unix)]`；各 AC 使用独立的新夹具）

- AC1：夹具按 #1014「Minimal reproduction」：提交 `a/b/c/d`、`a/b-2/c/d`、`a/x` 后执行 `rm -rf a/b && ln -s b-2 a/b && rm -rf a/b-2 && ln -s b a/b-2`；`libra status --short --no-branch` 须退出 0（执行条件）；stdout 行集合等于开工时以 pinned Git（`git@6de20f6092`）在同构状态下实测并写入本卡的 `git status --short --no-branch` 行集合（两侧均显式禁用分支行，不受 `status.branch` 配置影响，`src/command/status/input.rs:52-55`）（预期为 ` D a/b-2/c/d`、` D a/b/c/d`、`?? a/b`、`?? a/b-2`，以实测为准；抑制检查错误而跳过这些路径的错误实现缺少两条删除行）。
- AC2：提交 `d/f`（内容 `same`）后 `rm -rf d && ln -s <ext> d`，`<ext>/f` 内容同为 `same`；`libra status --short` 的 stdout 含 `d/f` 的删除行（短格式工作区列为 `D`）。
- AC3：提交 `d/f` 后 `rm -rf d && ln -s missing d`；`libra status --short` 须退出 0（执行条件），stdout 含 `d/f` 的删除行（短格式工作区列为 `D`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」。

**Verification:**

- [ ] 卡特有用例（AC1–AC3）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::status_test::status_does_not_traverse_leading_symlinks)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::status_test::/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-10, DEP-GII-03`

**Implementation write set:** `src/command/status.rs`, `src/command/status/scan.rs`, `src/command/status/output.rs`, `tests/command/status_test.rs`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/status.rs`, `src/command/status/scan.rs`, `src/command/status/output.rs`, `tests/command/status_test.rs`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`（已跟踪路径的前导分量为符号链接时视为删除）、`COMPATIBILITY.md`（原地更新 `status` 行，不改行数）；网站 `status.en.md` 由 `sync-libra-command-docs.py --only status` 生成。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=status扫描前导符号链接; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=GII-10,DEP-GII-03; writeset=序列化于GII-10; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-12: 支持 `<rev>...` 三点范围解析（`rev-parse`；#720 的范围部分）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 让 `libra rev-parse A...B`（任一侧省略时取 `HEAD`）按 Git 对称差语义输出 `<B_oid>`、`<A_oid>` 以及零个或全部最佳 merge-base 排除项 `^<mb_i>`。`checkout -b <branch> A...B` 等单一起点上下文的 merge-base 解析于 2026-11-18 按 G-09 拆出为 `GII-23`（追加于 Phase 2 末尾，链上紧随本卡）。

**Out of scope:** 单一起点解析（`GII-23`）；不改变 `log A...B` 的对称差遍历语义；`rev-parse --symmetric-difference` 等其它选项（尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `get_commit_base_typed` 不识别 `...`；已安装 0.30.207 实测 `libra rev-parse 'HEAD...HEAD'` → `not a valid object name`、`LBR-CLI-003`、退出 129 | `src/utils/util.rs:3128`、#720 |
| `merge-base --all` 已存在，可作测试内交叉核对 | `src/command/merge_base.rs:31` |

**Acceptance criteria:**

- [ ] AC1：存在唯一 merge-base `M` 时 `libra rev-parse A...B` 依次输出 `<B_oid>`、`<A_oid>`、`^<M_oid>`。
- [ ] AC2：`libra rev-parse A...` 以 `HEAD` 补全右侧。
- [ ] AC3：`libra rev-parse ...B` 以 `HEAD` 补全左侧。
- [ ] AC4：无共同祖先时 `libra rev-parse A...B` 的 stdout 逐字等于 `<B_oid>`、`<A_oid>` 两行（命令须退出 0）。
- [ ] AC5：criss-cross 历史存在多个最佳 merge-base 时，`libra rev-parse A...B` 的 stdout 由 `<B_oid>`、`<A_oid>` 两行及全部最佳 merge-base 的 `^<mb_i>` 行组成（命令须退出 0）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md` 与网站 `rev-parse.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::rev_parse_test::triple_dot_merge_base` 承载；期望顺序与集合由执行者在开工时以 pinned Git（`git@6de20f6092`）对同构历史预先核对并写入本卡记录，测试内以 Libra 自身 `merge-base --all` 输出交叉核对）

- AC1：退出 0；stdout 恰为三行，顺序与内容与 Git 对照一致。
- AC2：`HEAD` 指向 `B` 时 `libra rev-parse A...` 的 stdout 与 `libra rev-parse A...B` 相同。
- AC3：`HEAD` 指向 `A` 时 `libra rev-parse ...B` 的 stdout 与 `libra rev-parse A...B` 相同。
- AC4：两条无关根历史上命令须退出 0（执行条件）；stdout 逐字等于 `<B_oid>`、`<A_oid>` 两行。
- AC5：criss-cross 夹具按构造恰有两个最佳 merge-base：自根 `R` 分出改动不同文件的提交 `X`、`Y`，`A` 为第一父 `X`、第二父 `Y` 的合并提交，`B` 为第一父 `Y`、第二父 `X` 的合并提交，故最佳 merge-base 为 {`X`, `Y`}（开工时以 pinned Git 对同构历史核对 `git merge-base --all A B` 恰为这两项并写入本卡）；命令须退出 0（执行条件）；stdout 第 1、2 行依次为 `<B_oid>`、`<A_oid>`，第 3 行起各行（保留重复）排序后的列表等于 `^<X_oid>`、`^<Y_oid>` 排序后的列表（一次比较；期望来自夹具构造，不依赖 Libra 的 `merge-base` 实现；重复或遗漏任一 `^` 行的实现不满足）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::triple_dot_merge_base)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_rev_parse_peel_selectors`（整个 target；按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 触及共享 src/utils/util.rs 修订解析及版本面集合` | **Dependencies:** `GII-21, DEP-GII-03`

**Implementation write set:** `src/utils/util.rs`, `src/command/rev_parse.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/utils/util.rs`, `src/command/rev_parse.rs`, `tests/command/rev_parse_test.rs`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md`（原地更新 `rev-parse` 行，不改行数）；网站页由 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only rev-parse --libra-root /Volumes/Data/gitmono/libra` 生成。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=<rev>..._rev-parse三点范围解析; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=GII-21,DEP-GII-03; writeset=序列化于GII-21; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-13: 修复嵌套多路冲突外层标记宽度（9 → 7）与 `ours` 区域丢行（#1019）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 修复嵌套多路冲突物化（#1019 的 `mod6` rename/rename 链：`two`、`four`、`six` 各自含二路冲突且内含嵌套冲突区）时外层标记被加宽到 9 个字符、`ours` 区域丢失 stage-2 中一行（`20`）的缺陷，使外层标记为 7、嵌套标记保持 8、`ours` 区域与 stage-2 blob 逐行一致（匹配 Git 2.56.0）。

**Out of scope:** 不改变单层 `merge` / `diff3` / `zdiff3` 样式配置；criss-cross 虚拟 merge-base 内的嵌套标记加宽规则（既有 `merge_crisscross_nested_conflict_markers_are_wider_than_the_outer_ones` 覆盖，保持不变）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 外层标记长度 = `unambiguous_conflict_marker_length(...)` + `extra_marker_size` | `src/command/merge/content.rs:358-363` |
| `unambiguous_conflict_marker_length` 根据各侧内容中已有标记计算不歧义长度 | `src/command/merge/content.rs:954` |
| #1019 复现：0.30.91 外层 `<<<<<<<<<`（9），Git 为 7；`ours` 区域缺 `20` | #1019 |

**Acceptance criteria:**

- [ ] AC1：#1019 复现场景物化后的外层冲突标记为 7 个字符（`<<<<<<<`、`=======`、`>>>>>>>`）。
- [ ] AC2：嵌套冲突区的标记保持 8 个字符。
- [ ] AC3：外层冲突的 `ours` 区域与 stage-2 blob 逐行一致（含 `20`），无丢行。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件核对并同步 `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md` 与网站 `merge.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::merge_test::nested_multiway_conflict_preserves_ours_and_markers` 承载；夹具按 #1019：O `one=11..19`、`three=31..39`、`five=51..59`；A 改 `one=10..19`、`three+=40` 并 `one->two`、`three->four`、`five->six`；B 改 `one+=20`、`three+=forty`、`five+=60` 并 `one->six`、`three->two`、`five->four`；在 A 上 `libra merge B` 预期以冲突退出）

- AC1：`libra merge` 以冲突结束（非 0 退出，执行条件）；`two`、`four`、`six` 中外层冲突区的个数与开工时以 pinned Git 对同一夹具 `git merge` 实测并写入本卡的个数相同且每个文件 ≥ 1（执行条件，排除「渲染时丢掉冲突区」的错误实现）；这些外层区的每条标记行（`<<<<<<<`、`=======`、`>>>>>>>` 起始行）的标记字符连续段长度均为 7。
- AC2：上述文件中嵌套冲突区的个数与 pinned Git 实测个数相同且 ≥ 1（执行条件）；这些嵌套区的每条标记行（如 `<<<<<<<< HEAD:four`）的标记字符连续段长度均为 8。
- AC3：`two` 的外层 `ours` 区内容与 `libra show :2:two` 逐行相等（含 `20`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC3）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::nested_multiway_conflict_preserves_ours_and_markers)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::.*(conflict|crisscross)/)'`（含 `command::merge_test::merge_crisscross_nested_conflict_markers_are_wider_than_the_outer_ones`；按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-23, DEP-GII-03`

**Implementation write set:** `src/command/merge/content.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/content.rs`, `tests/command/merge_test.rs`

**Docs and compatibility impact:** 逐文件核对 `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md` 中嵌套冲突标记宽度描述（与新行为不符时同步改写，不改 `COMPATIBILITY.md` 行数；无需修改的文件在验收记录中写明「核对无需修改」及其 SHA-256）；网站 `merge.en.md` 由 `sync-libra-command-docs.py --only merge` 生成或经 `--check` 确认无漂移。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=嵌套多路冲突标记宽度与ours行保留; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=GII-23,DEP-GII-03; writeset=序列化于GII-23; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-14: `mv` 拒绝越过符号链接的目标，消除随之而来的 snapshot 树冲突（#1039）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** #1039 的 `tree path conflicts with file 'link'` 源于 `libra mv`：pinned 上游 `t7001-mv.sh` #23 在已提交 `from/src` 与 `from/link -> .` 的仓库中执行 `mv from/src from/link/real/dst`，期望以 `destination is beyond a symbolic link` 拒绝且不改 index；Libra 却把 index 条目改写为 `from/link/real/dst`，命令自身的 operation snapshot 随即以该树冲突失败，而 index 中残留的 `from/link`（`120000`）与 `from/link/real/dst` 使此后每条需要快照的命令（#24 起的 `reset --hard`、`mv -f`）以同一错误失败（TC-2033/2037）。本卡让 `mv` 在目标路径的任一前导分量为符号链接时按 Git 文案（`destination is beyond a symbolic link`）拒绝，`mv -f` 同样拒绝；两者都不改动 index 条目。#1039 中 #47/#48 的 `rm -fr .git && git init` 失败（TC-2038/2039）为同一测试文件前序状态的连锁结果：开工时按 pinned `t7001-mv.sh` 复刻 #47 的前序状态核对，若本修复后仍复现，先按 ER-03 修订本卡再开工。

**Out of scope:** 自动修复已被旧版本写坏的 index（尚未排期，重启条件：出现用户报告）；源路径越过符号链接的检查（上游另有用例，本卡只处理目标路径）；工作区扫描的前导符号链接与 `ELOOP`（`GII-11`、`GII-21`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 2026-11-18 以已安装 0.30.207 在仓库外临时目录复现：`mkdir from && echo contents >from/src && ln -s . from/link && libra add from/src from/link && libra commit -m s` 后，`libra mv from/src from/link/real/dst` 退出 129、`operation storage failed: snapshot object write failed: tree path conflicts with file 'link'`，`libra ls-files -s` 变为 `from/link`（`120000`）与 `from/link/real/dst`；之后 `libra reset --hard` 与 `libra mv -f from/src from/link/dst` 均以同一错误失败 | 本卡 `Verification` 复核记录、#1039 |
| `mv` 的拒绝经 `CliError::from_legacy_string` 输出 `fatal: ...`，如 `bad source` 为 `LBR-CLI-003`、退出 129 | `src/command/mv.rs:117-120,236-252` |
| 快照树构建 `insert_tree_path` 对文件/目录同名冲突报错 | `src/internal/operation/snapshot.rs:1131-1157` |
| 上游期望：#23 `test_must_fail git mv from/src from/link/real/dst` 且 `test_grep "destination is beyond a symbolic link"`、index 不变；#24 `mv -f` 同样拒绝 | `git@6de20f6092:t/t7001-mv.sh:186-208` |

**Acceptance criteria:**

- [ ] AC1：`from/link -> .` 时 `libra mv from/src from/link/real/dst` 以 `LBR-CLI-003` 失败。
- [ ] AC2：AC1 的失败后 index 条目不变（`libra ls-files -s` 的全量输出与执行前逐字节相同）。
- [ ] AC3：`libra mv -f` 对越过符号链接的目标同样以 `LBR-CLI-003` 拒绝。
- [ ] AC4：AC1 之后 `libra reset --hard` 退出 0。
- [ ] AC5：`insert_tree_path` 在路径前缀已是文件时返回 `conflicts with file` 错误。
- [ ] AC6：`insert_tree_path` 在目标已是目录时返回 `conflicts with directory` 错误。
- [ ] AC7：AC1 的 stderr 含 Git 文案 `destination is beyond a symbolic link`。
- [ ] AC8：AC3 的失败后 index 条目不变（`libra ls-files -s` 的全量输出与该命令执行前逐字节相同）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/mv.md`、`docs/commands/zh-CN/mv.md`、`docs/development/commands/mv.md`、`COMPATIBILITY.md` 与网站 `mv.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC1–AC4、AC7–AC8 由 `(new)` `command::mv_test::mv_refuses_destination_beyond_symlink` 承载，`#[cfg(unix)]`；AC5–AC6 由 `(new)` 单元用例 `internal::operation::snapshot::tests::insert_tree_path_rejects_real_file_directory_conflicts` 承载。夹具按 `t7001-mv.sh` #23：`mkdir from && echo contents >from/src && ln -s . from/link && libra add from/src from/link && libra commit -m "setup symlink case"`，记录 `libra ls-files -s` 为 `I0`）

- AC1：stderr 含 `Error-Code: LBR-CLI-003`。
- AC2：随后 `libra ls-files -s` 与 `I0` 逐字节相同。
- AC3：在 AC4 之后按 `t7001` #24 前序执行 `rm -rf from && mkdir from && echo contents >from/src && ln -s file from/link && libra add from/src from/link`；`libra mv -f from/src from/link/dst` 的 stderr 含 `Error-Code: LBR-CLI-003`。
- AC4：`libra reset --hard` 退出 0。
- AC5：同一 `(new)` 单元用例：先插入 `x` 再插入 `x/y`，返回错误且消息含 `conflicts with file`。
- AC6：同一 `(new)` 单元用例：先插入 `x/y` 再插入 `x`，返回错误且消息含 `conflicts with directory`。
- AC7：AC1 那次 `libra mv` 的 stderr 含 `destination is beyond a symbolic link`。
- AC8：AC3 的 `libra mv -f` 执行前记录 `libra ls-files -s` 为 `I1`，执行后同一输出与 `I1` 逐字节相同（先改写 index 再报错的错误实现会被识别）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」。

**Verification:**

- [ ] 卡特有用例（AC1–AC4、AC7–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::mv_test::mv_refuses_destination_beyond_symlink)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 卡特有用例（AC5–AC6）：`source .env.test && cargo nextest run --lib -E 'test(=internal::operation::snapshot::tests::insert_tree_path_rejects_real_file_directory_conflicts)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::mv_test::/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-13, DEP-GII-03`

**Implementation write set:** `src/command/mv.rs`, `src/internal/operation/snapshot.rs`（仅新增 `#[cfg(test)]` 单元用例）, `tests/command/mv_test.rs`, `docs/commands/mv.md`, `docs/commands/zh-CN/mv.md`, `docs/development/commands/mv.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/mv.rs`, `src/internal/operation/snapshot.rs`, `tests/command/mv_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/mv.md`、`docs/commands/zh-CN/mv.md`、`docs/development/commands/mv.md`（目标越过符号链接时拒绝，`-f` 不例外）、`COMPATIBILITY.md`（原地更新 `mv` 行，不改行数）；网站 `mv.en.md` 由 `sync-libra-command-docs.py --only mv` 生成。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=mv目标越过符号链接拒绝; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=GII-13,DEP-GII-03; writeset=序列化于GII-13; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-15: 修复仓库数据库繁忙时存储类型解析被误报为配置错误（#875）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** #875 的 tc1375 序列（`libra fetch remote_one`，远端为本地路径的镜像克隆）在 durable object-index repair marker 积压时间歇性失败：命令后台排空超出 60 秒预算后，`fetch` 解析对象存储时报 `failed to read object storage configuration for LIBRA_STORAGE_TYPE`（`LBR-IO-001`，退出 128）。源码路径：`fetch` → `util::resolve_configured_objects_storage` → `ClientStorage::resolve_configured_storage` → `read_storage_type()` → `resolve_env_sync_typed("LIBRA_STORAGE_TYPE")`；进程环境未设置该变量时，后者从仓库数据库读取 `vault.env.LIBRA_STORAGE_TYPE`，读取失败即被 `open_after_type_read` 映射为 `StorageConfigFailure{LIBRA_STORAGE_TYPE}`，而 object-index 回放可使仓库数据库（回滚日志模式）长时间持锁。本卡让该读取在仓库数据库暂时持锁时有界等待后正确解析，不再把暂时性锁竞争误报为存储配置错误；为使竞争可确定性复现，新增仅在 `debug_assertions` 且 `LIBRA_TEST=1` 时生效的故障注入点（与仓库既有 `LIBRA_TEST` 故障注入约定一致）。`cloud` / 非 dry-run `agent clean` 在积压未清时的 fail-closed（`LBR-IO-002`）保持不变。开工时先以 AC1 的用例在修复前代码上复现失败；若根因不是数据库持锁，先按 ER-03 修订本卡再实现。

**Out of scope:** 不削弱 `cloud` 与 `agent clean` 的 fail-closed 门禁；不改变 ADR-OI-04 的 marker 持久化格式；`StorageOpenError::Io` 被映射为 `StorageConfigFailure{objects}` 的另一分支（与 #875 现象不同，尚未排期）；#875 记录的另两种偶发结果（`no configured remote for the current branch`、`LBR-REPO-002 failed to inspect local repository state`）若在 AC3 循环中出现，按 ER-03 修订本卡或另立 `FIX-*`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `fetch` 调用 `util::resolve_configured_objects_storage()`，失败映射为 `FetchError::StorageConfig { variable }` → `IoReadFailed`（`LBR-IO-001`） | `src/command/fetch.rs:1026,1098-1099,2547-2550`、`src/utils/util.rs:1427-1430` |
| `resolve_env_sync_typed` 先读进程环境变量，未设置时经 `resolve_env_for_storage_init_typed` 读仓库数据库与全局配置中的 `vault.env.<name>`；`read_storage_type()` 的读取失败在 `open_after_type_read` 中被映射为 `StorageConfigFailure{LIBRA_STORAGE_TYPE}`；`StorageOpenError::Io` 则映射为 `variable: "objects"` | `src/utils/client_storage.rs:1216-1220,1793-1802,1830-1858,3510-3550` |
| object-index 更新可使仓库数据库持锁超过单次短 busy timeout（源码注释）；仓库数据库为回滚日志模式，既有用例以 `PRAGMA journal_mode = DELETE` + `BEGIN EXCLUSIVE` 模拟他连接持锁 | `src/utils/client_storage.rs:1172-1175`、`src/internal/db.rs:250,1322-1330` |
| 命令后台排空预算 60 秒，超时只告警；命令前置回放对 `cloud` / 非 dry-run `agent clean` 以 `IoWriteFailed`（`LBR-IO-002`）fail-closed | `src/cli.rs:2568-2638,4208-4232`、`src/utils/error.rs:392-393` |
| #875：0.30.91 上 tc1375 序列首次 `fetch` 以上述 `LBR-IO-001` 失败，下一条命令报 `replayed 100 durable cloud object-index repair marker(s), but more remain` | #875、`src/cli.rs:2613` |

**Acceptance criteria:**

- [ ] AC1：存储类型读取遇到他连接持有的仓库数据库排他锁时，`ClientStorage::resolve_configured_storage` 最终返回就绪存储（`Ok` 且 `configuration_variable()` 为 `None`），而非 `StorageConfigFailure`。
- [ ] AC2：故障注入使 `fetch` 的存储类型读取遇到持续 2 秒的仓库数据库排他锁时，`libra fetch remote_one` 退出 0。
- [ ] AC3：tc1375 序列在 20 个全新目录中各执行一次，`libra fetch remote_one` 全部退出 0。
- [ ] AC4：存在可回放 marker 积压时，`cloud` 命令的预检仍以 `LBR-IO-002` 失败。
- [ ] AC5：AC1 的用例在 `--stress-count 20` 下全部通过。
- [ ] AC6：仓库配置了可区分的存储设置（`s3` 类型、空 bucket）时，遇锁的 `resolve_configured_storage` 返回的结果反映锁释放后读取到的仓库配置（以 `LIBRA_STORAGE_BUCKET` 失败）。

**AC → 断言映射:**（AC1、AC2、AC6 的用例运行时进程环境不得含 `LIBRA_STORAGE_TYPE` 与 `LIBRA_STORAGE_BUCKET`——`Verification` 以 `env -u LIBRA_STORAGE_TYPE -u LIBRA_STORAGE_BUCKET` 启动，用例开头断言这两个变量均未设置，否则失败——以保证读取确实走仓库数据库）

- AC1 → `(new)` `utils::client_storage::tests::storage_type_read_waits_out_busy_repository_db`：本卡先只写入插桩——全局 `#[cfg(test)]` 原子计数 `STORAGE_TYPE_DB_READ_ATTEMPTS`（每次开始从仓库数据库读取存储类型时加 1）与该用例——在未做行为修复的树上运行，须失败（返回 `StorageConfigFailure`）并把输出记入本卡；若不失败，按 ER-03 修订本卡。随后写入行为修复再运行，须通过。用例步骤：在临时仓库中以独立 SQLite 连接执行 `PRAGMA journal_mode = DELETE` 与 `BEGIN EXCLUSIVE`（同 `src/internal/db.rs:1322-1330` 手法，锁由守卫对象持有，任何路径退出都会释放）；在另一线程调用 `ClientStorage::resolve_configured_storage(<objects>)`；主线程最多等待 10 秒直到计数 ≥ 1（超时即判失败并释放锁），再持锁 2 秒后提交释放；对调用线程 `join` 设 30 秒上限（超时判失败）；调用返回 `Ok`，且 `configuration_variable()` 为 `None`。该用例以 `utils::test::ConfigDbFixture`（`src/utils/test.rs:243-272`）隔离全局与系统配置并标注 `#[serial(env)]`，临时仓库不写入任何 `vault.env.LIBRA_STORAGE_*`，故锁释放后的正确读取结果是「未配置存储类型」，只能走本地分支（`src/utils/client_storage.rs:1836-1838`）。
- AC6 → `(new)` `utils::client_storage::tests::storage_type_read_after_lock_uses_repository_config`：与 AC1 相同的隔离（`ConfigDbFixture`、`#[serial(env)]`、进程环境无 `LIBRA_STORAGE_TYPE` / `LIBRA_STORAGE_BUCKET`）与相同的持锁、计数握手（最多等待 10 秒）、持锁 2 秒后提交释放与 30 秒 `join` 上限；临时仓库事先写入 `vault.env.LIBRA_STORAGE_TYPE=s3` 与 `vault.env.LIBRA_STORAGE_BUCKET=`（空串）。调用返回 `Err`，其 `variable` 为 `LIBRA_STORAGE_BUCKET`（`s3` 类型下空 bucket 的判定，`src/utils/client_storage.rs:1860-1872`）：遇锁即回落本地或不使用读取结果的实现返回 `Ok`，未修复的实现在锁内以 `LIBRA_STORAGE_TYPE` 失败，均不满足。
- AC2 → `(new)` `command::fetch_test::local_mirror_fetch_survives_storage_type_read_lock`：按 tc1375 建立 `<repo>` 与 `one`（`libra init` → `libra commit --allow-empty -m test` → `libra clone --mirror . one` → `libra remote add remote_one ./one` → 在 `one` 内 `libra tag tag_one` 与 `libra branch branch_one`）；以 `LIBRA_TEST=1`、`LIBRA_TEST_STORAGE_TYPE_READ_LOCK_MS=2000` 运行 `libra fetch remote_one`（该故障注入点在存储类型读取前由独立连接对仓库数据库取得排他锁并在 2 秒后释放，生效时向 stderr 写 `failpoint: storage-type-read-lock engaged`）：stderr 含该生效行（执行条件，证明命中目标读取）；`libra fetch remote_one` 退出 0。
- AC3 → `(new)` `command::fetch_test::local_mirror_fetch_never_reports_storage_config_error`：上述序列在 20 个全新临时目录中各执行一次（不设故障注入），每次 `libra fetch remote_one` 退出 0。
- AC4 → `(new)` `cli::tests::object_index_repair::pending_backlog_keeps_cloud_fail_closed`：预置 10 个可回放 marker（大于 `#[cfg(test)]` 页上限 3，`src/utils/client_storage.rs:218-227`）后对 `Commands::Cloud` 执行一次预检：返回错误，其 stable code 为 `LBR-IO-002`。
- AC5：按「验证执行协议」第 3a 条对 AC1 的用例执行 `--stress-count 20`。

**Verification:**

- [ ] 卡特有用例（AC1、AC4、AC6）：`source .env.test && env -u LIBRA_STORAGE_TYPE -u LIBRA_STORAGE_BUCKET cargo nextest run --lib -E 'test(=utils::client_storage::tests::storage_type_read_waits_out_busy_repository_db) or test(=utils::client_storage::tests::storage_type_read_after_lock_uses_repository_config) or test(=cli::tests::object_index_repair::pending_backlog_keeps_cloud_fail_closed)'`（均 `(new)`；`cargo nextest list` 须恰好列出这 3 条）
- [ ] 卡特有用例（AC2–AC3）：`source .env.test && env -u LIBRA_STORAGE_TYPE -u LIBRA_STORAGE_BUCKET cargo nextest run --test command_test -E 'test(=command::fetch_test::local_mirror_fetch_survives_storage_type_read_lock) or test(=command::fetch_test::local_mirror_fetch_never_reports_storage_config_error)'`（均 `(new)`；`cargo nextest list` 须恰好列出这 2 条）
- [ ] 卡特有压力门（AC5）：`source .env.test && env -u LIBRA_STORAGE_TYPE -u LIBRA_STORAGE_BUCKET cargo nextest run --lib --stress-count 20 -E 'test(=utils::client_storage::tests::storage_type_read_waits_out_busy_repository_db)'`（按「验证执行协议」第 3a 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::/) or test(/^utils::client_storage::tests::/)'`（按「验证执行协议」第 5 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::fetch_test::/) or test(/^command::cloud_test::/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 触及共享 client_storage 存储解析及版本面集合；新增 #[serial(env)] 用例并登记 tests/SERIAL_REGISTRY.tsv，同时命中 T-4` | **Dependencies:** `GII-14, DEP-GII-03`

**Implementation write set:** `src/utils/client_storage.rs`, `src/command/fetch.rs`, `src/cli.rs`（仅新增 `#[cfg(test)]` 用例）, `tests/command/fetch_test.rs`, `tests/SERIAL_REGISTRY.tsv`, `.config/nextest.toml`（按「全局工程约束」测试串行登记，由 `sh tests/NEXTEST_GROUPS.sh` 再生成）

**Release write set:** `Inherited` | **Files likely touched:** `src/utils/client_storage.rs`, `src/command/fetch.rs`, `src/cli.rs`, `tests/command/fetch_test.rs`, `tests/SERIAL_REGISTRY.tsv`, `.config/nextest.toml`

**Docs and compatibility impact:** `N/A`：修复内部存储解析的暂时性锁竞争误判，不改变公开 CLI 参数、输出格式或错误码定义（`LBR-IO-001` / `LBR-IO-002` 的含义不变）；新增的 `LIBRA_TEST_STORAGE_TYPE_READ_LOCK_MS` 仅在 debug 构建且 `LIBRA_TEST=1` 时生效，属测试注入点，不进入用户文档。

**Rollback mode:** `immutable-release` | **Estimated scope:** `S` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=仓库数据库繁忙时存储类型解析; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=2; prod-files=2; scope=S; deps=GII-14,DEP-GII-03; writeset=序列化于GII-14; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-16: `add` 的 object-index 更新改为批末批量回放（#611 第 2 项）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** `libra add` 当前在批末一次性持久化 repair marker（ADR-OI-04），但随后逐对象入队、由后台消费者逐个打开连接并提交事务（#611 以 `strace` 计每文件 2 次数据库打开、4 次 `fsync`）。本卡保持 marker 持久化语义不变，把批末的逐对象入队改为经既有批量回放路径 `ClientStorage::repair_pending_object_index_updates` 一次（或分页）回放本批 marker，并以固定基准证明 `add` 提速。本卡于 2026-11-18 按 G-09 拆出 `GII-18`（`fsck` 就地哈希）。

**Out of scope:** `fsck` 就地哈希（自本卡拆出，由 `GII-18` 承接）；#611 第 3 项 `benchmark/run.sh` 的 `--object-format` 与 `add_all` 场景（尚未排期，重启条件：基准脚本改造立项）；#611「Where add spends the rest」所列忽略规则重读、路径解析与线程池复用（尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `add` 以 `begin_object_index_batch` / `end_object_index_batch` / `flush_object_index_batch` 包裹对象写入 | `src/command/add.rs:594-608,1348-1350,1481-1483,1894-1895,2222-2224` |
| 批末 `flush_pending_object_index_batch` 一次持久化本批 marker，然后对每个对象 `enqueue_index_update` | `src/utils/client_storage.rs:1021-1033` |
| 后台消费者 `run_index_update_consumer` 逐条 `apply_queued_index_update`，失败时 `record_index_update_failure` 保留 marker 供重放 | `src/utils/client_storage.rs:1035-1066` |
| 批量回放入口 `ClientStorage::repair_pending_object_index_updates` 已用于命令前置修复 | `src/utils/client_storage.rs:2203-2215`、`src/cli.rs:2579-2638` |

**Acceptance criteria:**

- [ ] AC1：`libra add .` 成功返回后，`.libra/object-index-repair/` 中不残留本批 marker。
- [ ] AC2：`libra add .` 成功返回后，本批写入的每个对象在 object-index 中都有对应行。
- [ ] AC3：`libra add .` 的本批 object-index 更新经批量回放提交，而非逐对象入队（测试钩子报告的逐对象入队次数为 0、批量回放事务数等于 ⌈本批对象数 / 回放批大小⌉）。
- [ ] AC4：批量回放事务失败（故障注入）时 object-index 表无部分写入。
- [ ] AC5：AC4 的故障发生后，本批全部 marker 保留（ADR-OI-04 语义不变）。
- [ ] AC6：清除故障后的回放（重复至无剩余页）补齐 object-index。
- [ ] AC7：该回放完成后本批 marker 目录为空。
- [ ] AC8：固定基准协议达标：同机 `--release` 构建、确定性 2,000 个 1 KiB 文件、每轮从空索引重跑 `libra add .`、1 次预热 + 5 次计时取中位数，以 `GII-15` 完成提交为基线，`add` 中位耗时降低 `≥ 10%`（记录基线 SHA、硬件参数、两侧中位耗时与降幅）。

**AC → 断言映射:**

- AC1–AC3 → `(new)` `command::add_test::add_replays_object_index_markers_in_batches`：在 200 个文件的仓库上 `libra add .` 退出 0；随后 `.libra/object-index-repair/` 目录为空（AC1）；以只读 SQLite 连接查询 `.libra/libra.db` 的 `object_index` 表（`sql/sqlite_20260309_init.sql:60`），本批每个新 blob 的 OID 均有对应行（AC2）；以本卡新增、仅 debug 构建生效的测试钩子 `LIBRA_TEST_OBJECT_INDEX_REPLAY_STATS_PATH=<用例临时目录>/stats`（与 `src/cli.rs:4211-4216` 既有 `LIBRA_TEST_OBJECT_INDEX_GENERATION_LOCK_COUNT_PATH` 钩子同法，由 `add` 在结束前写出其暂存批——`begin_object_index_batch` 至 `flush_object_index_batch` 区间登记的本批对象，`src/command/add.rs:594-608`——的逐对象入队次数与回放该批 marker 的批量回放事务数；计数以该批标识隔离，不含 operation snapshot 在命令前后的捕获写入，`src/internal/operation/middleware.rs:445,685`）在另一同构新仓库运行同一 `libra add .`（须退出 0，执行条件），该文件内容逐字等于 `enqueued=0 replay_tx=2`（200 个 blob、非测试构建回放批大小 `INDEX_REPAIR_BATCH_SIZE = 100`，`src/utils/client_storage.rs:221-222`；仍逐对象入队的旧实现报告 `enqueued=200 replay_tx=0`）（AC3）。
- AC4–AC7 → `(new)` `utils::client_storage::tests::object_index_batch_replay_rolls_back_on_tx_failure`：夹具在 `object_index` 中预置至少 1 条既有行，并为本批恰好「`#[cfg(test)]` 下单个回放事务批的大小」个新对象持久化 marker（当前 `INDEX_REPAIR_BATCH_SIZE = 2`，`src/utils/client_storage.rs:221-224`；一次回放至多加载一批，`src/utils/client_storage.rs:4023-4029`；marker 为 `.libra/object-index-repair/<oid>.<type>.json`，`src/utils/client_storage.rs:747-766`），使一次回放恰为一个事务。注入前记录 `S0` = `SELECT id, o_id, o_type, o_size, repo_id, created_at, is_synced FROM object_index ORDER BY id` 的完整结果行列表，`K0` = marker 目录中每个文件的文件名及其内容字节；本卡在 `ObjectIndexTestFaults`（`src/utils/client_storage.rs:110-114`，经 `install_test_object_index_faults` 安装，`168-196`）中新增 `#[cfg(test)]` 故障字段与触达计数：在批量回放事务内、本批行的写入语句已执行之后与提交之前返回错误（既有 `update_failure` 在写入开始前即返回，`src/utils/client_storage.rs:3961-3964`，不能证明回滚，不得替代）；该次回放须返回 `Err` 且触达计数为 1（执行条件，证明失败发生在行已写入事务之后）。AC4：同一查询的完整结果行列表逐行等于 `S0`。AC5：marker 目录的文件名集合等于 `K0` 的文件名集合，且每个文件内容逐字节等于 `K0` 中的记录。清除故障（释放守卫）后重复调用 `repair_pending_object_index_updates`，直到返回的 `ObjectIndexRepairOutcome.remaining` 为 `false`（`src/utils/client_storage.rs:300-303`；每次须返回 `Ok`、至多 5 次，均为 AC6、AC7 的执行条件）。AC6：`K0` 中每个 marker 所记的 `o_id` 在 `object_index` 中各恰有一行，其 `o_type`、`o_size` 与该 marker 内容一致。AC7：上述回放完成后 marker 目录中不含任何文件。
- AC8 → 下方基准命令（非 nextest 门），判据为 `(基线中位 − 本卡中位) / 基线中位 ≥ 0.10`。

**Verification:**

- [ ] 卡特有用例（AC1–AC3）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::add_test::add_replays_object_index_markers_in_batches)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 卡特有用例（AC4–AC7）：`source .env.test && cargo nextest run --lib -E 'test(=utils::client_storage::tests::object_index_batch_replay_rolls_back_on_tx_failure)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 卡特有基准（AC8，非 nextest 门）：在仓库外临时目录 `<S>` 建 `<S>/base/` 与 `<S>/card/`，把 `GII-15` 完成提交上 `cargo build --release` 的产物复制为 `<S>/base/libra`、本卡最终树的产物复制为 `<S>/card/libra`（文件名必须保持 `libra`：状态辅助进程按 `std::env::current_exe()` 自举，见 `src/internal/worktree_io/executor.rs:442` 与 #611 记录的改名失败），在仓库根先后以 `LIBRA_BENCH_BIN=<S>/base/libra` 与 `LIBRA_BENCH_BIN=<S>/card/libra` 各运行一次 `python3 -I -c "import os,tempfile,pathlib,subprocess,time,statistics,shutil; b=os.environ['LIBRA_BENCH_BIN']; d=pathlib.Path(tempfile.mkdtemp()); h=d/'home'; h.mkdir(); env=dict(os.environ,HOME=str(h),LIBRA_CONFIG_GLOBAL_DB=str(h/'global.db')); w=d/'w'; w.mkdir(); [(w/f'f_{i:04d}.txt').write_bytes(f'file-{i:04d}-'.encode()+b'x'*1014) for i in range(2000)]; q=dict(cwd=w,env=env,check=True,stdout=subprocess.DEVNULL); t=[]; [(shutil.rmtree(w/'.libra',ignore_errors=True), subprocess.run([b,'init'],**q), (t0:=time.perf_counter()), subprocess.run([b,'add','.'],**q), t.append(time.perf_counter()-t0)) for _ in range(6)]; print('add_median_s=',statistics.median(t[1:])); shutil.rmtree(d)"`（脚本不吞 stderr：任一夹具步骤失败即抛出 `CalledProcessError` 并以非零退出，该次不得记为测量值；两次运行同机、同电源状态，记录两侧 `add_median_s`）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::add_test::/) or test(/^command::fsck_test::/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 触及共享 client_storage.rs object-index 及版本面集合` | **Dependencies:** `GII-15, DEP-GII-03`

**Implementation write set:** `src/command/add.rs`, `src/utils/client_storage.rs`, `tests/command/add_test.rs`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/add.rs`, `src/utils/client_storage.rs`, `tests/command/add_test.rs`

**Docs and compatibility impact:** `N/A`：内部热路径性能优化，命令参数、输出与错误码均不变。

**Performance budget:** 见 AC8；不得引入与文件数相关的额外数据库打开或无界内存（GC-10）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=add_object-index批末批量回放; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=2; prod-files=2; scope=M; deps=GII-15,DEP-GII-03; writeset=序列化于GII-15; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-17: Hermes extraction 下沉至 `observed_agents` 并收口 #656

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** #656 报告的守卫失败（上游 `760a5529` 新增 `AgentKind::Hermes` 分支使 `extraction.rs` 非测试 `AgentKind::` 路径升至 16 > 15）在当前源码中已被字符串分派绕开：`extraction.rs` 以 `agent_kind.as_db_str() == "hermes"` 进入文件内的 `hermes_bounded_extraction`，非测试 `AgentKind::` 路径为 14。按 `ADR-GII-02`，本卡把 Hermes 有界抽取实现下沉到 `observed_agents/extract.rs`（与 `extract_pi_bounded` 同层），使 `extraction.rs` 不再含 Hermes 专用实现与字符串字面量分派，同时保持守卫冻结基线不升、Hermes 抽取行为不变，然后收口 #656。

**Out of scope:** 不修改 `tests/compat/agent_architecture_guard.rs` 的冻结常数 `15`（`ADR-GII-02`）；Hermes 实时读取桥 `observed_agents/hermes_export.rs`（与抽取无关，永久非目标）；其它 provider 的抽取分派。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 守卫冻结 `("src/internal/ai/capture/extraction.rs", 15)`，基线只许下降 | `tests/compat/agent_architecture_guard.rs:5846-5859` |
| `extraction.rs` 非测试 `AgentKind::` 路径共 14 处（行 95、262、263、266、269、425、430、433、441、566、752、753、754、756） | `src/internal/ai/capture/extraction.rs` |
| Hermes 分派为字符串比较，实现位于本文件并调用 `observed_agents::builtin::hermes::hermes_worker_frame_authorized` | `src/internal/ai/capture/extraction.rs:421-422,457-473` |
| Pi 的有界抽取已位于 `observed_agents/extract.rs` 并由 `AgentKind::Pi` 分支调用 | `src/internal/ai/observed_agents/extract.rs:816`、`src/internal/ai/capture/extraction.rs:441` |

**Acceptance criteria:**

- [ ] AC1：`src/internal/ai/capture/extraction.rs` 的非测试部分不再含 Hermes 专用实现：相对本卡开工基线删除 `hermes_bounded_extraction`、不新增任何函数定义，且不再引用 Hermes provider 模块（`hermes::` 路径或 `hermes_worker_frame_authorized`）。
- [ ] AC2：`src/internal/ai/capture/extraction.rs` 的非测试部分没有任何按 Rust 转义规则解码后含 `hermes`（不区分大小写）的字符串字面量（字符串分派不再存在）。
- [ ] AC3：Hermes 有界抽取由 `observed_agents/extract.rs` 承担，对授权 worker frame 的抽取结果（`prompts`、`partial`）与迁移前实现的结果相等。
- [ ] AC4：合成或解析失败的 Hermes frame 的抽取结果与迁移前实现的结果相等（迁移前为 `partial=true`、无 prompts）。
- [ ] AC5：`compat_agent_architecture_guard` 的 `live_capture_shared_runtime_is_provider_neutral` 通过（该用例断言 `extraction.rs` 非测试 `AgentKind::` 计数不超过冻结上限 15）。
- [ ] AC6：本卡不修改 `tests/compat/agent_architecture_guard.rs`（冻结常数保持 `15`）。
- [ ] AC7：经共享入口 `deadline_provider_extraction_summary(AgentKind::Hermes, …)` 对授权 Hermes frame 的抽取结果（`prompts`、`partial`）与迁移前实现的结果相等。

**AC → 断言映射:**

- AC1–AC2 → 下方「GII-17 结构守卫脚本」（非 nextest 门）：只检查文件中 `#[cfg(test)]\nmod tests` 之前的非测试部分（2026-11-18 位于第 836 行），以单遍词法扫描同时识别注释、普通 / 字节 / C 字符串、原始字符串与字符字面量（字符串先于注释被识别，故 `"https://…"` 中的 `//` 不会被当作注释）；函数集合与模块引用只在去掉注释和字面量后的代码上检查。`new fns` 非空、`hermes_bounded_extraction kept` 为 `True` 或 `hermes module refs` 大于 0 即 AC1 失败，`hermes literals` 非空即 AC2 失败（字面量按 Rust 规则解码，含 `\x`、`\u{…}` 转义）。2026-11-18 核对：当前源码输出 `hermes_bounded_extraction kept: True`、`hermes literals: ['hermes']`、`hermes module refs: 3` 并退出 1（`src/internal/ai/capture/extraction.rs:421-422,457-473`）；删除 Hermes 分支与实现后退出 0；分别加入 `"her\x6des"` 字符串分派、`r#"hermes"#` 原始字符串分派、`"https://hermes.example/api"` 字面量或改名的新函数后退出 1；只在注释中提到 hermes 时退出 0。
- AC3–AC4 → `(new)` `internal::ai::observed_agents::extract::tests::hermes_bounded_extraction_matches_previous_projection`：以迁移前实现的输出为期望（固定 fixture：一条含 ordinary prompts 的授权 frame、一条合成 frame、一条非法字节），断言 `prompts` 列表与 `partial` 标志逐项相等。
- AC5 → `live_capture_shared_runtime_is_provider_neutral`：该用例通过（其内部断言计数 ≤ 15）；记录输出行 `src/internal/ai/capture/extraction.rs: <n> non-test AgentKind:: paths (baseline 15)` 中的 `<n>`。
- AC6 → 下方差异门（非 nextest 门）：提交前以本卡开工基线提交为 `--old`、工作区为新侧，该文件无差异；提交后对已提交树复核一次。
- AC7 → `(new)` `internal::ai::capture::extraction::tests::hermes_shared_entry_matches_previous_projection`：以 AC3 的授权 frame 调用 `deadline_provider_extraction_summary(AgentKind::Hermes, …)`（`src/internal/ai/capture/extraction.rs:405-410`），`prompts` 列表与 `partial` 标志与迁移前实现的输出逐项相等（删除旧分派后落入默认分支的错误实现返回 `partial=true` 且无 prompts，`src/internal/ai/capture/extraction.rs:450-453`）。


**GII-17 结构守卫脚本**（`Verification` 第 1 项使用；`sys.argv[1]` 为本卡开工基线提交）：

```python
import re, subprocess, sys
P = 'src/internal/ai/capture/extraction.rs'
TOKEN = re.compile(r'''(?P<lc>//[^\n]*)|(?P<bc>/\*.*?\*/)|(?P<raw>\b[bc]?r(?P<h>\#*)"(?P<rb>.*?)"(?P=h))|(?P<str>\b[bc]?"(?P<sb>(?:[^"\\]|\\.)*)"|"(?P<sb2>(?:[^"\\]|\\.)*)")|(?P<ch>'(?:\\.|[^'\\\n])')''', re.S)
ESC = {'n': '\n', 't': '\t', 'r': '\r', '0': '\0', '\\': '\\', '"': '"', "'": "'"}
def decode(body):
    body = re.sub(r'\\\n\s*', '', body)
    body = re.sub(r'\\u\{([0-9a-fA-F]+)\}', lambda m: chr(int(m.group(1), 16)), body)
    body = re.sub(r'\\x([0-9a-fA-F]{2})', lambda m: chr(int(m.group(1), 16)), body)
    return re.sub(r'\\(.)', lambda m: ESC.get(m.group(1), m.group(1)), body)
def lex(src):
    k = src.find('\n#[cfg(test)]\nmod tests')
    if k < 0:
        sys.exit(2)
    src = src[:k]
    code, lits, pos = [], [], 0
    for m in TOKEN.finditer(src):
        code.append(src[pos:m.start()])
        code.append(' ')
        pos = m.end()
        if m.group('raw') is not None:
            lits.append(m.group('rb'))
        elif m.group('str') is not None:
            lits.append(decode(m.group('sb') if m.group('sb') is not None else m.group('sb2')))
    code.append(src[pos:])
    return ''.join(code), lits
def fns(code):
    return set(re.findall(r'\bfn\s+([A-Za-z_][A-Za-z0-9_]*)', code))
try:
    base_src = subprocess.run(['libra', 'show', sys.argv[1] + ':' + P], capture_output=True, text=True, check=True).stdout
    cur_src = open(P, encoding='utf-8').read()
except Exception:
    sys.exit(2)
base_code, _ = lex(base_src)
cur_code, cur_lits = lex(cur_src)
new_fns = sorted(fns(cur_code) - fns(base_code))
kept = 'hermes_bounded_extraction' in fns(cur_code)
lits = [s for s in cur_lits if 'hermes' in s.lower()]
mods = len(re.findall(r'\bhermes\s*::|builtin\s*::\s*hermes|hermes_worker_frame_authorized', cur_code))
print('new fns:', new_fns, '| hermes_bounded_extraction kept:', kept, '| hermes literals:', lits, '| hermes module refs:', mods)
sys.exit(1 if (new_fns or kept or lits or mods) else 0)
```

**Verification:**

- [ ] 卡特有守卫（AC1–AC2，非 nextest 门；退出 0 = 全部零命中，1 = 至少一项命中，2 = 读取基线或当前文件失败、或找不到测试模块分界，即执行失败）：在仓库根执行 `python3 -I - <本卡开工基线提交>`，以 here-doc 传入下方「GII-17 结构守卫脚本」（脚本只读，不写任何文件）。
- [ ] 卡特有用例（AC3–AC4）：`source .env.test && cargo nextest run --lib -E 'test(=internal::ai::observed_agents::extract::tests::hermes_bounded_extraction_matches_previous_projection)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 卡特有用例（AC7）：`source .env.test && cargo nextest run --lib -E 'test(=internal::ai::capture::extraction::tests::hermes_shared_entry_matches_previous_projection)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 卡特有用例（AC5）：`source .env.test && cargo nextest run --test compat_agent_architecture_guard --no-capture -E 'test(=live_capture_shared_runtime_is_provider_neutral)'`（既有用例；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 卡特有差异门（AC6，非 nextest 门）：提交前运行 `libra diff --old <本卡开工基线提交> --exit-code -s -- tests/compat/agent_architecture_guard.rs`（`--new` 缺省为工作区；退出 0 即无差异，退出 1 判 AC6 失败）；提交后以 `libra diff --old <本卡开工基线提交> --new HEAD --exit-code -s -- tests/compat/agent_architecture_guard.rs` 复核
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_agent_architecture_guard`（整个 target；按「验证执行协议」第 5 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^internal::ai::capture::extraction::tests::/) or test(/^internal::ai::observed_agents::extract::tests::/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-16, DEP-GII-03`

**Implementation write set:** `src/internal/ai/capture/extraction.rs`, `src/internal/ai/observed_agents/extract.rs`

**Release write set:** `Inherited` | **Files likely touched:** `src/internal/ai/capture/extraction.rs`, `src/internal/ai/observed_agents/extract.rs`

**Docs and compatibility impact:** `N/A`：内部架构中立性重构，无公开 CLI、输出或文档变化。

**Rollback mode:** `immutable-release` | **Estimated scope:** `S` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=Hermes_extraction下沉至observed_agents; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=5/8; landing=2; prod-files=2; scope=S; deps=GII-16,DEP-GII-03; writeset=序列化于GII-16; release=independent; split-from=N/A; exception=N/A`

---

### Task GII-18: `fsck` 对象校验就地哈希（#611 第 1 项，自 GII-16 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** `fsck::verify_object` 在校验对象哈希前把 `"<type> <size>\0"` 头与对象内容复制进新的 `payload` 缓冲再整体求摘要；本卡改为调用已就地实现的 `crate::utils::object::git_object_hash`（以增量哈希器先后写入头与借用的内容，不分配整对象拷贝，`src/utils/object.rs:1062-1072`）计算摘要，校验结果逐字节不变，并以固定基准证明 `fsck` 提速。拆分原因：与 `GII-16` 的 object-index 批量回放属不同恢复轴（纯 CPU 路径 vs 持久化/事务语义），按 G-01/G-09 自 `GII-16` 拆出并追加于 Phase 2 末尾。

**Out of scope:** `add` 的 object-index 批量回放（`GII-16`）；#611 第 1 项中 fetch 路径的 `verify_fetched_object`（`src/utils/storage/tiered.rs:40`）与流式校验器（尚未排期，重启条件：fetch 性能计划立项）；`utils::object::git_object_hash`（已就地，`src/utils/object.rs:1062-1072`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `verify_object` 构造 `header` 后 `Vec::with_capacity` + 两次 `extend_from_slice` 生成 `payload`，再 `object_format::digest(kind, &payload)` | `src/command/fsck.rs:2388,2489-2498` |
| `git_object_hash_for_kind` 已以 `HashAlgorithm::new_for_kind` 先后 `update(header)`、`update(data)` | `src/utils/object.rs:1062-1072` |

**Acceptance criteria:**

- [ ] AC1：`sha1` 对象格式下，`verify_object` 对以原实现摘要为 OID 存放的非空 blob 返回 `CheckStatus::Ok`。
- [ ] AC2：`sha256` 对象格式下，`verify_object` 对以原实现摘要为 OID 存放的非空 blob 返回 `CheckStatus::Ok`。
- [ ] AC3：`blake3` 对象格式下，`verify_object` 对以原实现摘要为 OID 存放的非空 blob 返回 `CheckStatus::Ok`。
- [ ] AC4：零长度对象（空 blob，`sha1`）同样使 `verify_object` 返回 `CheckStatus::Ok`。
- [ ] AC5：可正常解码但内容与其路径 OID 不符的松散对象使 `verify_object` 走摘要比较失败分支（结果的 `error_message` 以 `Hash mismatch: expected` 开头）。
- [ ] AC6：固定基准协议达标：同机 `--release` 构建、确定性 2,000 个 1 KiB 文件的已提交仓库、1 次预热 + 5 次计时取中位数，以 `GII-17` 完成提交为基线，`fsck` 中位耗时降低 `≥ 5%`（记录基线 SHA、硬件参数、两侧中位耗时与降幅）。
- [ ] AC7：`verify_object` 以 `crate::utils::object::git_object_hash` 对借用的对象内容 `&data` 计算摘要，函数体内不再调用 `object_format::digest`。

**AC → 断言映射:**（对象类型只影响头部文本 `<type> <size>\0`，新旧实现以同一 `header` 字符串构造，故以 blob 为规范场景；AC1–AC5 直接调用生产函数 `verify_object`（`src/command/fsck.rs:2388`），期望 OID 在测试内按原实现计算——`<type> <size>\0` 与内容拼接后经 `crate::internal::object_format::digest`——并按该 OID 把对象写入临时仓库的松散对象存储）

- AC1 → `(new)` `command::fsck::tests::verify_object_in_place_hash_matches_concatenated_payload`：在 `set_hash_kind_for_test(HashKind::Sha1)` 下写入 blob `hello\n` 后调用 `verify_object`，返回结果的 `status` 为 `CheckStatus::Ok`（摘要与原实现不同的错误实现返回 `HashMismatch`）。
- AC2 → 同一用例：`HashKind::Sha256` 下同一 blob，`status` 为 `CheckStatus::Ok`。
- AC3 → 同一用例：`HashKind::Blake3` 下同一 blob，`status` 为 `CheckStatus::Ok`。
- AC4 → 同一用例：`HashKind::Sha1` 下空 blob（头部 `blob 0\0`），`status` 为 `CheckStatus::Ok`。
- AC5 → 同一用例：`HashKind::Sha1` 下把另一内容（`world\n`）的合法 zlib 松散对象写到 blob `hello\n` 的 OID 路径（可正常读取与解码，内容与 OID 不符）后调用 `verify_object`，返回结果的 `error_message` 以 `Hash mismatch: expected` 开头（摘要比较分支，`src/command/fsck.rs:2503-2520`；读取失败分支的 `Failed to read object` 文案不满足，`src/command/fsck.rs:2468-2482`）。
- AC6 → 下方基准命令（非 nextest 门），判据为 `(基线中位 − 本卡中位) / 基线中位 ≥ 0.05`。
- AC7 → 下方结构守卫（非 nextest 门）：取 `async fn verify_object(` 至其后首个列首 `}` 行的函数体，须含以 `&data` 为内容参数的 `git_object_hash(…, &data)` 调用且不含 `object_format::digest(`（仍拼接 `payload` 后整体求摘要、或把拷贝缓冲传给 `git_object_hash` 的实现不满足）。2026-11-18 在当前源码上运行：`in-place call: False`、`object_format::digest in body: True`，退出 1（`src/command/fsck.rs:2493-2498`）。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --lib -E 'test(=command::fsck::tests::verify_object_in_place_hash_matches_concatenated_payload)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 卡特有守卫（AC7，非 nextest 门；退出 0 = 满足，1 = AC7 失败，2 = 找不到函数体即执行失败）：`python3 -I -c "import re,sys; s=open('src/command/fsck.rs',encoding='utf-8').read(); i=s.find('async fn verify_object('); j=s.find('\n}\n',i) if i>=0 else -1; (i<0 or j<0) and sys.exit(2); b=s[i:j]; inplace=bool(re.search(r'git_object_hash\(\s*[^,]+,\s*&data\s*\)', b)); legacy='object_format::digest(' in b; print('in-place call:', inplace, '| object_format::digest in body:', legacy); sys.exit(0 if (inplace and not legacy) else 1)"`
- [ ] 卡特有基准（AC6，非 nextest 门）：在仓库外临时目录 `<S>` 建 `<S>/base/` 与 `<S>/card/`，把 `GII-17` 完成提交上 `cargo build --release` 的产物复制为 `<S>/base/libra`、本卡最终树的产物复制为 `<S>/card/libra`（文件名必须保持 `libra`，原因同 `GII-16`：`src/internal/worktree_io/executor.rs:442`），在仓库根先后以 `LIBRA_BENCH_BIN=<S>/base/libra` 与 `LIBRA_BENCH_BIN=<S>/card/libra` 各运行一次 `python3 -I -c "import os,tempfile,pathlib,subprocess,time,statistics,shutil; b=os.environ['LIBRA_BENCH_BIN']; d=pathlib.Path(tempfile.mkdtemp()); h=d/'home'; h.mkdir(); env=dict(os.environ,HOME=str(h),LIBRA_CONFIG_GLOBAL_DB=str(h/'global.db')); w=d/'w'; w.mkdir(); [(w/f'f_{i:04d}.txt').write_bytes(f'file-{i:04d}-'.encode()+b'x'*1014) for i in range(2000)]; q=dict(cwd=w,env=env,check=True,stdout=subprocess.DEVNULL); [subprocess.run([b,*a],**q) for a in (['init'],['add','.'],['config','user.name','Bench'],['config','user.email','bench@example.com'],['commit','--no-gpg-sign','-m','bench'])]; t=[]; [((t0:=time.perf_counter()), subprocess.run([b,'fsck'],**q), t.append(time.perf_counter()-t0)) for _ in range(6)]; print('fsck_median_s=',statistics.median(t[1:])); shutil.rmtree(d)"`（脚本不吞 stderr：任一夹具步骤失败即以非零退出，该次不得记为测量值；两次运行同机、同电源状态，记录两侧 `fsck_median_s`）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::fsck_test::/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `GII-17, DEP-GII-03`

**Implementation write set:** `src/command/fsck.rs`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/fsck.rs`

**Docs and compatibility impact:** `N/A`：内部热路径性能优化，`fsck` 参数、输出与错误码均不变。

**Performance budget:** 见 AC6；不得增加每对象的内存分配（GC-10）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `S` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fsck对象校验就地哈希; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=3/8; landing=1; prod-files=1; scope=S; deps=GII-17,DEP-GII-03; writeset=序列化于GII-17; release=independent; split-from=GII-16; exception=N/A`

---

### Task GII-21: operation snapshot 工作区扫描不经前导符号链接（#1014 的 `add`/`commit`/`reset`/`checkout`/`merge` 部分，自 GII-11 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** #1014 复现状态下，`add`、`commit`、`reset --hard`、`checkout` 以及 `t6415` #23 的合并都在 operation snapshot 的工作区扫描中经前导符号链接分量解析已跟踪路径并触发 `ELOOP`，以 `fatal: operation storage failed: working-copy scan failed`（`LBR-INTERNAL-001`）中止。本卡让 `scan_working_copy` 对已跟踪路径做与 `GII-11` 相同的前导符号链接检测（Git `has_symlink_leading_path` 语义），并让最终分量为符号链接的条目以不跟随链接的元数据与链接目标字节计算 blob（`hash_file` 现以 `fs::metadata` 跟随最终链接，互指链接得 `ELOOP`、悬空链接得 `NotFound` 而使扫描失败或不完整，`src/internal/operation/snapshot.rs:703-704`），使这些命令与 Git 2.56.0 一样完成。拆分原因（G-09）：原 `GII-11` 按独立谓词计数超过 G-03 上限；`status` 扫描与 operation snapshot 扫描是两个独立实现、可独立恢复的轴，本卡追加于 Phase 2 末尾、在执行链上紧随 `GII-11`。

**Out of scope:** `status` 扫描（`GII-11`）；`mv` 越过符号链接写出 index 同名冲突（#1039，`GII-14`）；#1008（另一 issue，不在本计划）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| operation snapshot 的 `scan_working_copy` 以 `fs::symlink_metadata(&absolute)` 检查条目，I/O 错误映射为 `ScanError::Io`（`working-copy scan failed`） | `src/internal/operation/snapshot.rs:106-115,256-340,660` |
| #1014：复现状态下 `add -A`、`commit`、`reset --hard`、`checkout main` 均以 `LBR-INTERNAL-001` 失败；`t6415` #23 合并后同样失败 | #1014 |
| 上游合并场景：`t6415-merge-dir-to-symlink.sh` #23（`merge should not have F/D conflicts (recursive)`）及其前序 #1、#5、#18 | `git@6de20f6092:t/t6415-merge-dir-to-symlink.sh:9-21,49-56,134-174` |

**Acceptance criteria:**

- [ ] AC1：在 #1014 复现状态下执行 `libra add -A` 后，`libra ls-files -s a/b a/b-2` 的输出逐字等于映射中的期望文本（仅 `a/b`、`a/b-2` 两条 `120000` 条目）。
- [ ] AC2：AC1 之后 `libra commit -m loop` 创建提交。
- [ ] AC3：在 #1014 复现状态下 `libra reset --hard` 退出 0。
- [ ] AC4：在 #1014 复现状态下 `libra checkout main` 退出 0。
- [ ] AC5：`t6415` #23 场景（`checkout -b foo test2` 后 `merge -s recursive baseline^0`）合并后，`a/b/c/d` 为普通文件。
- [ ] AC6：同一合并后，`a/b-2` 为符号链接。
- [ ] AC7：已跟踪目录被替换为悬空符号链接时，operation snapshot 的工作区扫描 `scan_working_copy` 返回的完整 `ScanResult` 等于期望快照。
- [ ] AC8：在 #1014 复现状态下，operation snapshot 的工作区扫描 `scan_working_copy` 返回的完整 `ScanResult` 等于期望快照。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/{op,add,commit,reset,checkout,merge}.md`、`docs/commands/zh-CN/{op,add,commit,reset,checkout,merge}.md`、`docs/development/commands/{op,add,commit,reset,checkout,merge}.md`、`COMPATIBILITY.md` 与网站 `op.en.md`、`add.en.md`、`commit.en.md`、`reset.en.md`、`checkout.en.md`、`merge.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（AC1–AC4 由 `(new)` `command::op_test::snapshot_scan_does_not_traverse_leading_symlinks` 承载；AC5–AC6 由 `(new)` `command::merge_test::dir_to_symlink_merge_keeps_nested_file` 承载；AC7 由 `(new)` 单元用例 `internal::operation::snapshot::tests::scan_completes_full_on_dangling_symlink`、AC8 由 `(new)` 单元用例 `internal::operation::snapshot::tests::scan_completes_full_on_leading_symlink_loop` 承载；均 `#[cfg(unix)]`。#1014 复现夹具：提交 `a/b/c/d`、`a/b-2/c/d`、`a/x` 后执行 `rm -rf a/b && ln -s b-2 a/b && rm -rf a/b-2 && ln -s b a/b-2`；AC3、AC4 各用独立的新夹具，AC7 在单元用例的临时仓库中构造悬空链接夹具，AC8 在单元用例的临时仓库中按同一 #1014 夹具构造）

- AC1：`libra add -A` 须退出 0（否则本 AC 失败）；`libra ls-files -s a/b a/b-2` 的 stdout 逐字等于两行 `120000 <O1> 0\ta/b` 与 `120000 <O2> 0\ta/b-2`（`<O1>`、`<O2>` 分别为内容 `b-2`、`b`（无换行）的 blob OID，测试以 `libra hash-object --stdin` 计算）。
- AC2：`libra commit -m loop` 须退出 0（否则本 AC 失败）；`libra rev-parse HEAD` 相对提交前前进一步。
- AC3：`libra reset --hard` 退出 0。
- AC4：`libra checkout main` 退出 0。
- AC5：按 pinned `t6415` 前序复刻：提交 `a/b/c/d`、`a/b-2/c/d`、`a/x` 并打 `start`；把 `a/b` 换成指向 `b-2` 的链接并提交；`reset --hard` 后改 `a/x` 提交并打 `baseline`；从 `start` 把 `a/b-2` 换成指向 `b` 的链接并提交、打 `test2`；`libra checkout -b foo test2` 与 `libra merge -s recursive baseline^0` 均须退出 0（否则本 AC 失败）；随后 `a/b/c/d` 的 `symlink_metadata` 为普通文件。
- AC6：同一测试中 AC5 的合并之后，`a/b-2` 的 `symlink_metadata` 为符号链接（合并须已退出 0）。
- AC7：单元用例的临时仓库只提交 `a/b/c/d`，再执行 `rm -rf a/b && ln -s missing a/b`；以与 AC8 相同的方式（`CapturePolicy::TrackedAndUntracked`、`with_stat_short_circuit(false)`）构造扫描器，`scan_working_copy` 返回 `Ok`（执行条件），且整个 `ScanResult` 与期望一次比较相等：`tracked` = ∅、`untracked` = {`a/b` → 链接目标 `missing` 的 blob OID}、`completeness` = `Completeness::Full`、`bytes` = 7、`reused_index_oid` = ∅（以跟随最终链接的 `fs::metadata` 得 `NotFound` 后把扫描降级为 `Partial` 的现实现不满足，`src/internal/operation/snapshot.rs:372-379,703-704`）。
- AC8：本卡先只写入该单元用例，在未修复的树上运行须失败（`scan_working_copy` 以 `ScanError::Io` 失败；把输出记入本卡；若不失败，按 ER-03 修订本卡后再实现）。修复后：在临时仓库按 #1014 复现夹具构造工作区，调用 `WorkspaceSnapshotter::scan_working_copy`（`src/internal/operation/snapshot.rs:256`）返回 `Ok`（执行条件），且以 `CapturePolicy::TrackedAndUntracked`（`src/internal/operation/view.rs:25-29`）并 `with_stat_short_circuit(false)`（`src/internal/operation/snapshot.rs:221-224`，使复用集合确定为空）构造的扫描器返回的整个 `ScanResult`（`src/internal/operation/snapshot.rs:142-150`，派生 `PartialEq`）与期望值一次比较相等：`tracked` = {`a/x` → 内容 `x\n` 的 blob OID}（被前导符号链接遮蔽的 `a/b/c/d`、`a/b-2/c/d` 不在其中）、`untracked` = {`a/b` → 链接目标 `b-2` 的 blob OID，`a/b-2` → 链接目标 `b` 的 blob OID}、`completeness` = `Completeness::Full`、`bytes` = 6（`a/x` 的 2 字节加两个链接目标长度 3 与 1）、`reused_index_oid` = ∅（夹具中 `a/x` 内容为 `x\n`；期望 OID 在测试内按内容计算；漏掉 `a/x` 或链接的实现、或把检查错误降级为 `Partial` 而吞掉的错误实现——同文件 `complete = false; continue` 的既有降级写法——都不满足）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::op_test::snapshot_scan_does_not_traverse_leading_symlinks) or test(=command::merge_test::dir_to_symlink_merge_keeps_nested_file)'`（均 `(new)`；`cargo nextest list` 须恰好列出这 2 条）
- [ ] 卡特有用例（AC7）：`source .env.test && cargo nextest run --lib -E 'test(=internal::operation::snapshot::tests::scan_completes_full_on_dangling_symlink)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 卡特有用例（AC8）：`source .env.test && cargo nextest run --lib -E 'test(=internal::operation::snapshot::tests::scan_completes_full_on_leading_symlink_loop)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^internal::operation::snapshot::/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 触及共享 operation snapshot 工作区扫描及版本面集合` | **Dependencies:** `GII-11, DEP-GII-03`

**Implementation write set:** `src/internal/operation/snapshot.rs`, `tests/command/op_test.rs`, `tests/command/merge_test.rs`, `docs/commands/op.md`, `docs/commands/add.md`, `docs/commands/commit.md`, `docs/commands/reset.md`, `docs/commands/checkout.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/op.md`, `docs/commands/zh-CN/add.md`, `docs/commands/zh-CN/commit.md`, `docs/commands/zh-CN/reset.md`, `docs/commands/zh-CN/checkout.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/op.md`, `docs/development/commands/add.md`, `docs/development/commands/commit.md`, `docs/development/commands/reset.md`, `docs/development/commands/checkout.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/op.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/add.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/internal/operation/snapshot.rs`, `tests/command/op_test.rs`, `tests/command/merge_test.rs`, `docs/commands/op.md`, `docs/commands/zh-CN/op.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 本卡把 `add`、`commit`、`reset`、`checkout`、`merge` 在「已跟踪目录被替换为互指符号链接」工作区形态下的致命中止（`working-copy scan failed`）改为与 Git 一致地完成，并使「已跟踪目录被替换为悬空符号链接」时的快照不再降级为不完整，属用户可见行为与错误变化（ER-06a）。逐文件更新：`docs/commands/op.md`、`docs/commands/zh-CN/op.md`、`docs/development/commands/op.md`（operation snapshot 工作区扫描对已跟踪路径不经前导符号链接分量：该路径按符号链接（`120000`）记录，其下的已跟踪路径视为已删除，不经该链接把它们记为存在）；`docs/commands/{add,commit,reset,checkout,merge}.md`、`docs/commands/zh-CN/{add,commit,reset,checkout,merge}.md`、`docs/development/commands/{add,commit,reset,checkout,merge}.md`（各补一句该形态下命令按 Git 语义完成，并链接 `op` 页的扫描说明）；`COMPATIBILITY.md` 原地更新 `op`、`add`、`commit`、`reset`、`checkout` 行与 `merge` 的 `partial` 行（不改行数）；网站 `op.en.md`、`add.en.md`、`commit.en.md`、`reset.en.md`、`checkout.en.md`、`merge.en.md` 由 `sync-libra-command-docs.py --only op add commit reset checkout merge` 生成。错误码不变（`docs/error-codes.md`：`N/A`）。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=operation_snapshot扫描前导符号链接; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=1; scope=M; deps=GII-11,DEP-GII-03; writeset=序列化于GII-11; release=independent; split-from=GII-11; exception=N/A`

---

### Task GII-23: `checkout -b <branch> A...B` 单一起点 merge-base 解析（#720 的单一起点部分，自 GII-12 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 让经 `util::get_commit_base_typed`（`src/utils/util.rs:3128`）解析单一提交的上下文（规范场景 `libra checkout -b <branch> A...B`，上游 t2018#4 TC-0390）把 `A...B`（右侧省略的 `A...` 取 `HEAD`，即上游 t2018#4 的 `branch1...` 形式）解析为唯一 merge-base 并照常创建与切换分支；0 个或多个最佳 merge-base 时按 Git fail-closed，且失败时不创建分支、不改动 HEAD、index 与工作区。拆分原因（G-09）：原 `GII-12` 计入 `checkout` 的切换与失败无副作用谓词后超过 G-03 上限；范围解析与单一起点解析是两个可独立恢复的轴，本卡追加于 Phase 2 末尾、在执行链上紧随 `GII-12`。

**Out of scope:** `rev-parse` 范围解析（`GII-12`）；左侧省略的 `...B` 单一起点形式（本卡不承诺其行为，尚未排期）；其它经 `get_commit_base_typed` 的命令中 `A...B` 的行为不另设验收（尚未排期）；不改变 `log A...B` 的对称差遍历语义。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `get_commit_base_typed` 不识别 `...` | `src/utils/util.rs:3128`、#720 |
| `checkout -b <branch> branch1...` 被当作 pathspec（#720，上游 t2018#4 TC-0390） | `src/command/checkout.rs`、#720 |
| `merge-base --all` 已存在，可作测试内交叉核对 | `src/command/merge_base.rs:31` |

**Acceptance criteria:**

- [ ] AC1：唯一 merge-base `M` 时 `libra checkout -b topic A...B` 创建的 `topic` 指向 `M`。
- [ ] AC2：AC1 的命令之后 HEAD 指向 `refs/heads/topic`。
- [ ] AC3：AC1 的命令之后工作区与 index 均与 `M` 的树一致。
- [ ] AC4：无共同祖先时 `libra checkout -b topic A...B` 以 `LBR-CLI-003` 失败。
- [ ] AC5：AC4 的失败前后仓库状态快照 `RS` 逐字节相同。
- [ ] AC6：存在多个最佳 merge-base 时 `libra checkout -b topic A...B` 以 `LBR-CLI-003` 失败。
- [ ] AC7：AC6 的失败前后仓库状态快照 `RS` 逐字节相同。
- [ ] AC8：HEAD 指向 `B` 时 `libra checkout -b topic2 A...`（右侧省略取 `HEAD`）创建的 `topic2` 指向 `M`。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`COMPATIBILITY.md` 与网站 `checkout.en.md`，按「文档与网站验收命令」逐步验收。

**AC → 断言映射:**（均由 `(new)` `command::checkout_test::checkout_b_triple_dot_merge_base` 承载。唯一 merge-base 夹具：提交 `m.txt` 得 `M`，分支 `a` 在 `M` 上加 `a.txt` 得 `A`，分支 `b` 在 `M` 上加 `b.txt` 得 `B`，执行前 HEAD 指向 `refs/heads/a`（`A` ≠ `M`）；无关历史夹具与 criss-cross 夹具（自根 `R` 分出改动不同文件的提交 `X`、`Y`，`A` = 以 `X`、`Y` 为父的合并提交，`B` = 以 `Y`、`X` 为父的合并提交，按构造有两个最佳 merge-base）各用独立的新仓库。仓库状态快照 `RS` = `libra show-ref` 的 stdout、`libra symbolic-ref HEAD` 的 stdout、`.libra/index` 的 SHA-256 与工作区全部条目（排除 `.libra`）以 `lstat`（不跟随符号链接）取得的「路径、类型（普通文件 / 符号链接 / 目录）、权限位、普通文件内容的 SHA-256 或符号链接目标」按路径排序的列表，按此顺序拼接的文本）

- AC1：`libra checkout -b topic A...B` 退出 0（执行条件）；`libra rev-parse topic` 等于 `M`（以 `HEAD` 为起点的错误实现得到 `A`）。
- AC2：同一命令之后 `libra symbolic-ref HEAD` 输出 `refs/heads/topic`（只建分支不切换的错误实现仍输出 `refs/heads/a`）。
- AC3：同一命令之后 `libra status --porcelain` 输出为空（未把工作区与 index 切换到 `M` 的错误实现会残留 `a.txt` 的差异）。
- AC4：无关历史夹具上 stderr 含 `Error-Code: LBR-CLI-003`。
- AC5：同一夹具上该命令执行前后各取 `RS`，逐字节相同（先创建或切换 `topic` 再报错的错误实现会改变 `show-ref` 或 HEAD）。
- AC6：criss-cross 夹具上 stderr 含 `Error-Code: LBR-CLI-003`。
- AC7：同一夹具上该命令执行前后各取 `RS`，逐字节相同。
- AC8：另一同构的新夹具中先 `libra checkout b`（执行条件，HEAD 指向 `refs/heads/b` 即 `B`）；`libra checkout -b topic2 a...` 退出 0（执行条件）；`libra rev-parse topic2` 等于 `M`（把 `a...` 当作 `a` 的错误实现得到 `A`；不识别右侧省略的实现会报错）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–7 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::checkout_test::checkout_b_triple_dot_merge_base)'`（`(new)`；`cargo nextest list` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（按「验证执行协议」第 5 条）

**Full-suite trigger:** `T-1: 触及共享 src/utils/util.rs 修订解析及版本面集合` | **Dependencies:** `GII-12, DEP-GII-03`

**Implementation write set:** `src/utils/util.rs`, `src/command/checkout.rs`, `tests/command/checkout_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/utils/util.rs`, `src/command/checkout.rs`, `tests/command/checkout_test.rs`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`（`checkout -b <branch> A...B` 以唯一 merge-base 为起点，0 个或多个 merge-base 时以 `LBR-CLI-003` 拒绝且不改动仓库）、`COMPATIBILITY.md`（原地更新 `checkout` 行，不改行数）；网站 `checkout.en.md` 由 `sync-libra-command-docs.py --only checkout` 生成。按「文档与网站验收命令」完成全部步骤。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=checkout_-b_A...B单一起点merge-base解析; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=GII-12,DEP-GII-03; writeset=序列化于GII-12; release=independent; split-from=GII-12; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `cli::tests`、`utils::util::test`、`utils::client_storage::tests`、`command::fsck::tests`、`internal::operation::snapshot`、`internal::ai::observed_agents::extract::tests`、`internal::ai::capture::extraction::tests` | `source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::/) or test(/^utils::util::test::/) or test(/^utils::client_storage::tests::/) or test(/^command::fsck::tests::/) or test(/^internal::operation::snapshot::/) or test(/^internal::ai::observed_agents::extract::tests::/) or test(/^internal::ai::capture::extraction::tests::/)'` |
| 集成 | `init`、`status`、`add`、`fsck`、`merge`、`rev_parse`、`checkout`、`mv`、`config`、`fetch`、`cloud`、`op` | `source .env.test && cargo nextest run --test command_test -E 'test(/^command::init_test::/) or test(/^command::status_test::/) or test(/^command::add_test::/) or test(/^command::fsck_test::/) or test(/^command::merge_test::/) or test(/^command::rev_parse_test::/) or test(/^command::checkout_test::/) or test(/^command::mv_test::/) or test(/^command::config_test::/) or test(/^command::fetch_test::/) or test(/^command::cloud_test::/) or test(/^command::op_test::/)'` |
| 兼容 | `agent_architecture_guard`、`rev_parse_peel_selectors` | `source .env.test && cargo nextest run --test compat_agent_architecture_guard --test compat_rev_parse_peel_selectors` |
| 迁移 | N/A（无 SQLite schema 变更） | N/A |
| 安全 | `--template` 自引用、循环 / `!` shell `alias.*`、循环与前导符号链接、`mv` 越过符号链接、`extensions.*` fail-closed、`GIT_DIR` 不回落 cwd | 同上单元与集成用例（各卡卡特有用例） |
| 性能 | `add` 中位降幅 `≥ 10%`（`GII-16`）、`fsck` 中位降幅 `≥ 5%`（`GII-18`） | 两卡 `Verification` 中的固定基准命令（同机基线对照） |
| live/gated | 每卡 T-1 与收口阶段双环境全量回归 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

上表过滤式只用 nextest 的 `or` 关键字组合，不含 `|` 与反斜杠转义，可直接复制执行。

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| GII-01 | #686；`src/cli.rs:234-336`、`src/utils/output.rs:250-327` | `src/cli.rs`, `src/main.rs`, `src/utils/output.rs` | `docs/commands/README.md`、`docs/commands/zh-CN/README.md`、`COMPATIBILITY.md`、网站 `index.en.mdx` | `command::init_test::global_dash_c_directory` `(new)` |
| GII-02 | #801（等号形式）；`src/cli.rs:234-336`、`src/utils/util.rs:1262-1269` | `src/cli.rs`, `src/main.rs`, `src/utils/output.rs`, `src/utils/util.rs` | 同 GII-01 | `command::status_test::global_git_dir_and_work_tree` `(new)` |
| GII-03 | #987；`src/utils/util.rs:1262-1269,1464` | `src/cli.rs`, `src/main.rs`, `src/utils/util.rs` | 同 GII-01 | `command::status_test::git_dir_env_overrides_cwd` `(new)` |
| GII-04 | #944；`src/command/init.rs:336-340`、`src/command/rev_parse.rs:900-914` | `src/cli.rs`, `src/command/init.rs`, `src/command/rev_parse.rs`, `src/utils/util.rs` | `docs/commands/{README,init}.md`、`docs/commands/zh-CN/{README,init}.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md`、网站 `index.en.mdx` / `init.en.md` | `command::init_test::global_bare_init` `(new)` |
| GII-05 | #945；`src/cli.rs:355,3176-3210` | `src/cli.rs`, `src/main.rs`, `src/internal/config.rs`, `src/utils/output.rs` | `docs/commands/{README,config}.md`、`docs/commands/zh-CN/{README,config}.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、网站 `index.en.mdx` / `config.en.md` | `command::config_test::git_alias_expansion` `(new)` |
| GII-06 | #942；`src/command/init.rs:105,197-200,722-726` | `src/command/init.rs` | `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md`、网站 `init.en.md` | `command::init_test::missing_template_warns_and_succeeds` `(new)`、`command::init_test::test_init_template_self_reference_guard` |
| GII-07 | #943；`src/utils/shared_perm.rs:32-60,205-237` | 无代码改动（`audit`） | `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md`、网站 `init.en.md` | 本卡 `Verification` 第 1 项的复现脚本输出（手工证据） |
| GII-08 | #946；`src/command/init.rs:1426` | `src/command/init.rs` | 同 GII-06 | `command::init_test::reinit_ignored_initial_branch_warning` `(new)` |
| GII-09 | #948；`src/cli.rs:95-170,1889,3687`、`src/command/init.rs:2373-2378` | `src/cli.rs`, `src/command/init.rs`, `src/internal/config.rs` | `docs/commands/{init,config}.md`、`docs/commands/zh-CN/{init,config}.md`、`docs/development/commands/{init,config}.md`、`COMPATIBILITY.md`、网站 `init.en.md` / `config.en.md` | `command::init_test::repositoryformatversion_and_extensions_validation` `(new)` |
| GII-10 | #834；`src/command/status/input.rs:27` | `src/command/status/input.rs`, `src/command/status.rs`, `src/command/status/output.rs` | `docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`COMPATIBILITY.md`、网站 `status.en.md` | `command::status_test::verbose_staged_and_unstaged_diff` `(new)` |
| GII-11 | #1014；`src/command/status/scan.rs:443,600`、`src/command/status.rs:745,1342` | `src/command/status.rs`, `src/command/status/scan.rs`, `src/command/status/output.rs` | 同 GII-10 | `command::status_test::status_does_not_traverse_leading_symlinks` `(new)` |
| GII-12 | #720（范围部分）；`src/utils/util.rs:3128` | `src/utils/util.rs`, `src/command/rev_parse.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md`、网站 `rev-parse.en.md` | `command::rev_parse_test::triple_dot_merge_base` `(new)` |
| GII-13 | #1019；`src/command/merge/content.rs:358-363,954` | `src/command/merge/content.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、网站 `merge.en.md` | `command::merge_test::nested_multiway_conflict_preserves_ours_and_markers` `(new)` |
| GII-14 | #1039；`src/command/mv.rs`、`src/internal/operation/snapshot.rs:1131-1157`、`git@6de20f6092:t/t7001-mv.sh:186-208` | `src/command/mv.rs` | `docs/commands/mv.md`、`docs/commands/zh-CN/mv.md`、`docs/development/commands/mv.md`、`COMPATIBILITY.md`、网站 `mv.en.md` | `command::mv_test::mv_refuses_destination_beyond_symlink` `(new)`、`internal::operation::snapshot::tests::insert_tree_path_rejects_real_file_directory_conflicts` `(new)` |
| GII-15 | #875；`src/utils/client_storage.rs:1216-1220,1830-1858,3510-3550`、`src/command/fetch.rs:1098-1099,2547-2550` | `src/utils/client_storage.rs`, `src/command/fetch.rs` | N/A（内部修复） | `utils::client_storage::tests::storage_type_read_waits_out_busy_repository_db` `(new)`、`utils::client_storage::tests::storage_type_read_after_lock_uses_repository_config` `(new)`、`cli::tests::object_index_repair::pending_backlog_keeps_cloud_fail_closed` `(new)`、`command::fetch_test::{local_mirror_fetch_survives_storage_type_read_lock,local_mirror_fetch_never_reports_storage_config_error}` `(new)` |
| GII-16 | #611 第 2 项；`src/utils/client_storage.rs:1021-1066` | `src/command/add.rs`, `src/utils/client_storage.rs` | N/A（内部性能） | `command::add_test::add_replays_object_index_markers_in_batches` `(new)`、`utils::client_storage::tests::object_index_batch_replay_rolls_back_on_tx_failure` `(new)`、`add` 基准 |
| GII-17 | #656；`src/internal/ai/capture/extraction.rs:421-422,457-473` | `src/internal/ai/capture/extraction.rs`, `src/internal/ai/observed_agents/extract.rs` | N/A（内部架构守卫） | `internal::ai::observed_agents::extract::tests::hermes_bounded_extraction_matches_previous_projection` `(new)`、`compat_agent_architecture_guard::live_capture_shared_runtime_is_provider_neutral`、`internal::ai::capture::extraction::tests::hermes_shared_entry_matches_previous_projection` `(new)` |
| GII-18 | #611 第 1 项；`src/command/fsck.rs:2489-2498` | `src/command/fsck.rs` | N/A（内部性能） | `command::fsck::tests::verify_object_in_place_hash_matches_concatenated_payload` `(new)`、`fsck` 基准 |
| GII-19 | #686；`src/cli.rs:234-336`、`src/utils/output.rs:250-327` | `src/cli.rs`, `src/main.rs`, `src/utils/output.rs` | 同 GII-01 | `command::init_test::global_dash_c_interleaves_with_global_flags` `(new)` |
| GII-20 | #948；`src/cli.rs:95-170` | `src/cli.rs`, `src/internal/config.rs` | `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md`、网站 `init.en.md` | `command::init_test::refstorage_extension_validation` `(new)` |
| GII-21 | #1014；`src/internal/operation/snapshot.rs:106-115,256-340`、`git@6de20f6092:t/t6415-merge-dir-to-symlink.sh:166-174` | `src/internal/operation/snapshot.rs` | `docs/commands/{op,add,commit,reset,checkout,merge}.md`、`docs/commands/zh-CN/` 同名文件、`docs/development/commands/` 同名文件、`COMPATIBILITY.md`、网站 `op`/`add`/`commit`/`reset`/`checkout`/`merge` `.en.md` | `command::op_test::snapshot_scan_does_not_traverse_leading_symlinks` `(new)`、`command::merge_test::dir_to_symlink_merge_keeps_nested_file` `(new)`、`internal::operation::snapshot::tests::scan_completes_full_on_dangling_symlink` `(new)`、`internal::operation::snapshot::tests::scan_completes_full_on_leading_symlink_loop` `(new)` |
| GII-22 | #948；`src/cli.rs:95-170`、`src/internal/object_format.rs:20-27` | `src/cli.rs`, `src/internal/config.rs` | `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`COMPATIBILITY.md`、网站 `init.en.md` | `command::init_test::objectformat_extension_value_validation` `(new)` |
| GII-23 | #720（单一起点部分）；`src/utils/util.rs:3128`、上游 t2018#4 | `src/utils/util.rs`, `src/command/checkout.rs` | `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`COMPATIBILITY.md`、网站 `checkout.en.md` | `command::checkout_test::checkout_b_triple_dot_merge_base` `(new)` |
| GII-24 | #801（空格分隔形式与缺值）；`src/cli.rs:234-336`、`src/main.rs:699-703` | `src/cli.rs`, `src/main.rs`, `src/utils/output.rs` | 同 GII-01 | `command::status_test::global_git_dir_and_work_tree_space_form` `(new)` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划文本双审通过且 `DEP-GII-01` 满足 | Claude（Subagent）与 Codex 对同一受审 SHA-256 `PASS` | N/A |
| M1 | Phase 0（`GII-01`..`GII-05`、`GII-19`、`GII-24`）全部 `done`/`complete`（正式登记 `DEFER-*` 的卡除外） | 各卡 `patch + 1`、`nextest` 与 D 组全绿 | 发布前 revert；发布后按 ER-10 新建 `FIX-*` 前滚发新版本 |
| M2 | Phase 1（`GII-06`..`GII-09`、`GII-20`、`GII-22`）全部 `done`/`complete`（正式登记 `DEFER-*` 的卡除外） | 同上 | 同上 |
| M3 | Phase 2（`GII-10`..`GII-18`、`GII-21`、`GII-23`）全部 `done`/`complete`（正式登记 `DEFER-*` 的卡除外）+ 全量收口门通过 | 全量 `nextest` + `fmt` + `clippy` 全绿 | 同上 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| `GII-16` 批量 object-index 写入中断 | 未提交事务 | SQLite 事务回滚，索引保持原子一致 | 部分更新导致索引损坏 |
| 远端 D 组 CI 失败 | 已推送版本 tag 与提交 | 按 ER-04/ER-10 新建 `FIX-*` 前滚修复 | 回退已推送提交或删除已发布 tag |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `issues/529.md` 释放 `src/cli.rs` 前抢跑 | 高：合并冲突 | 严格执行 `DEP-GII-01` 门禁 | GII-01..GII-05, GII-19, GII-24, GII-09, GII-22, GII-20, GII-15 |
| `GII-09` 严格校验误拒既有仓库 | 高：既有 Libra 仓库无法打开 | 只对 `extensions.*` 存在或版本 > 1 的情形拒绝；既有默认仓库（版本 0、仅 `core.objectformat`）由共享回归组 `test(/^command::init_test::.*object_format/)` 覆盖，`GII-09` AC4 断言一致的 v1 + `extensions.objectformat` 仓库照常工作 | GII-09 |
| 基准阈值受机器噪声或已落地收益影响 | 中：性能卡无法达标 | 同机、同电源状态、1 次预热 + 5 次中位数；未达标时卡保持未完成并修订计划，不得静默下调阈值 | GII-16, GII-18 |
| `GII-17` 误改冻结基线常数 | 高：违反 `ADR-ACF-10` | 仅下沉适配器，不修改 `agent_architecture_guard.rs` 常数 | GII-17 |
| 同批计划并发修改相交文件（`COMPATIBILITY.md`、`src/cli.rs` 等） | 高：合并冲突或覆盖他人改动 | 每卡开工编辑前满足 `DEP-GII-03`（无交集或已获协调者串行窗口） | GII-01..GII-24 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| `add` object-index 更新 | 每批一次（或分页）回放 | 数据库打开与提交次数与文件数解耦 | `add` 中位耗时降幅 `≥ 10%`（2,000 × 1 KiB，同机基线对照） | GII-16 基准 |
| `fsck` 对象哈希 | 每对象零额外整对象拷贝 | `O(总对象字节)` | `fsck` 中位耗时降幅 `≥ 5%`（同上夹具） | GII-18 基准 |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 对应命令已同步。
- [ ] `docs/commands/{init,config,status,rev-parse,checkout,merge,mv,op,add,commit,reset,README}.md` 及 `zh-CN/` 已同步。
- [ ] `docs/development/commands/{init,config,status,rev-parse,checkout,merge,mv,op,add,commit,reset}.md` 已同步。
- [ ] `/Volumes/Data/gitmono/libra-backend` 已按「文档与网站验收命令」第 2–7 步同步（子命令页经 `sync-libra-command-docs.py --write --only <cmd> --libra-root /Volumes/Data/gitmono/libra`，全局标志经 `index.en.mdx`）；变更的网站文件已逐文件核验部署，未变更的网站文件已记录第 3 步证据。
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
| 2026-11-18 R1 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `51d0a469d255f83052978b817dcb11495606091707239bfee7ae73f2ee06c569` | `FAIL` | P1×18：验证协议 `S=0` 不可满足；6 条前缀过滤零匹配；多匹配过滤误标「恰好 1」；GII-15/17 测试路径不可匹配；测试矩阵 `\|`；AC 映射套话；「文档与网站验收命令」缺失；GII-12/15/17 计数不符；`AGENTS.md:135` 归属矛盾与 R5 历史被改写；10:35 未来时间；GII-16 前提错误；GII-07/#943 已修复；GII-17 计数已为 14；GII-11 夹具不复现 #1014 且漏 snapshot 扫描；GII-05 `st` 与内置别名冲突；GII-10 锚点/写集错；GII-09 漏 #948 核心规则；追溯表退化 —— 均于 R1 修订行关闭 | P2×5（锚点漂移、Phase 1 进入条件、性能数字不一致、完成判据缺项、历史行乱序/状态行陈旧）：前四项已修，历史行顺序保留原样（只追加不重排） | R2 复审 |
| 2026-11-18 R1 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `51d0a469d255f83052978b817dcb11495606091707239bfee7ae73f2ee06c569` | `FAIL` | P1×10：`S=0` 不可满足；过滤零匹配/误标恰好 1；GII-15/17 测试路径；测试矩阵反斜杠；AC 映射矛盾；文档验收块缺失；GII-12/15/17 计数；`AGENTS.md` 归属与 R5 历史改写；未来时间戳；GII-16 前提 —— 均于 R1 修订行关闭 | 无 | R2 复审 |
| 2026-11-18 R2 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `a1323b5c7232d709929ba75562bfb4b5e3d785520d964881dfb7de57c8258a9a` | `FAIL` | P1×4：18 处 `---` 前缺空行致 setext 标题；`GII-09` 校验阻断 `libra config` 无修复通道；`GII-02` AC6 缺值用例错误；`ADR-GII-01` 别名查找未排除内置别名且引用过期行号 —— 均于 R2 修订行关闭 | P2×4：均已修 | R3 复审 |
| 2026-11-18 R2 | Codex (`gpt-6.1-terra`, high) | 受审 SHA-256 `a1323b5c7232d709929ba75562bfb4b5e3d785520d964881dfb7de57c8258a9a` | `FAIL` | P1×1：评审期间文件被修订（读到 `07573bfa29c1…`），无法对受审字节给出结论 —— 已改为评审期间冻结文件 | 无 | R3 复审 |
| 2026-11-18 R3 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `8069ec0159edee762b78180857a3f51390bf2e0978a8daa5114697297f89eac7` | `FAIL` | P1×4：`GII-15` 错把预检当失败点且错用 `LBR-IO-001`（实为 `fetch` 存储解析误判，`cloud` 为 `LBR-IO-002`）；`GII-11` 修复手段与根因不符（前导符号链接）；`-C ''` 与 Git 语义相反；`GII-14` 缺具体复现 —— 均于 R3 修订行关闭（`GII-14` 经复现改为 `mv` 修复） | P2×1（测试矩阵缺 `fetch_test`）：已修 | R4 复审 |
| 2026-11-18 R3 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `8069ec0159edee762b78180857a3f51390bf2e0978a8daa5114697297f89eac7` | `FAIL` | P1×10：位置参数 `libra diff` 树核验；`ls -d .libra .git` 退出 2；部署核验未绑定本次改动；压力门计数矛盾；`GII-01` 基线在提交后比较；`GII-09` 修复检查不全且缺 `refstorage`；`GII-11` 缺 `checkout`/合并/缺失目标；`GII-07` 缺裸仓库；基准二进制改名与吞 stderr；AC 按行而非谓词计数 —— 均于 R3 修订行关闭 | 无 | R4 复审 |
| 2026-11-18 R4 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `928e3021aeb015233f5d8799edde7d03a267da9e0487b20216117d339f72bbeb` | `FAIL` | P1×1：第 7 步零命中未按退出码模板且 `curl \| rg` 管道吞掉 `curl` 失败 —— 于 R4 修订行关闭；另独立确认 Codex R4 六项 | P2×2（`GII-06` AC1 复合、成功定义过强）：已修 | R5 复审 |
| 2026-11-18 R4 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `928e3021aeb015233f5d8799edde7d03a267da9e0487b20216117d339f72bbeb` | `FAIL` | P1×6：G-03 仍按行计数（含附加断言）；网站门无法完成无变更交付；`GII-17` AC6 依赖尚不存在的最终提交；`GII-15` 未把积压/存储解析/`fetch` 关联起来且源码归因有误；`GII-07` 非裸映射自相矛盾；`GII-11` 缺失目标用例未经前导符号链接 —— 均于 R4 修订行关闭 | 无 | R5 复审 |
| 2026-11-18 R5 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `b9751f0b14e0359b2f116e42b65635b2671808f19b4618682de8eada3bdce121` | `FAIL` | P1×1：`GII-18` 映射含 AC 之外的 `HashMismatch` 断言 —— 于 R5 修订行关闭；另独立确认 Codex R5 六项 | P2×1（`GII-15` 未要求修复前先失败、未写明持锁手法）：已修 | R6 复审 |
| 2026-11-18 R5 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `b9751f0b14e0359b2f116e42b65635b2671808f19b4618682de8eada3bdce121` | `FAIL` | P1×6：网站无变更分支与完成门冲突；`GII-15` 锁竞争测试无握手且受环境变量绕过；`GII-01` `--` 断言不能证明未切换目录；`GII-01`/`GII-19` 拆分使首版留下 `-C --json` 误判；独立谓词计数仍未关闭（`GII-12`/`GII-14`）；`VER` 把共享回归门计入 —— 均于 R5 修订行关闭 | 无 | R6 复审 |
| 2026-11-18 R6 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `bec2834230bc32e2777762edb598bd77a40e61dcb903e1f4c03d6c795aace4c5` | `PASS` | 无 P0/P1（事后核对：漏判 Codex R6 的 6 项 P1，已在本行补记并随 R6 修订行关闭） | P2×3（ADR-GII-01 结果段、`GII-14` AC3 错误码、风险登记过期引用）：已修 | R7 复审 |
| 2026-11-18 R6 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `bec2834230bc32e2777762edb598bd77a40e61dcb903e1f4c03d6c795aace4c5` | `FAIL` | P1×6：网站无变更分支可跳过未部署的工作区改动；`GII-15` 修复前复现依赖修复引入的握手且等待无界；`GII-09` 承诺缺验收且漏 `config` 文档；`GII-08/13/14` 含公开行为却标 `S`；`GII-07` 为独立补测试碎片卡；`GII-11` 缺不穿透的可观察验收 —— 均于 R6 修订行关闭 | 无 | R7 复审 |
| 2026-11-18 R7 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `38551546341e1856a5bb91defeb4667378486d15494d82f057429ee2a91e44b4` | `FAIL` | P1×1：网站第 3 步把「目标文件无他人未提交改动」「本地 `cf` 等于 `origin/cf`」只作无变更分支条件，有变更分支会混入他人改动或推送他人提交 —— 于 R7 修订行关闭 | P2×1（`GII-07` AC11 复合两条断言）：已修 | R8 复审 |
| 2026-11-18 R7 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `38551546341e1856a5bb91defeb4667378486d15494d82f057429ee2a91e44b4` | `FAIL` | P1×6：`GII-07` / `GII-08` 完成死锁；网站对账、无变更与部署恢复路径缺失且线上只核关键词；网站路径替换缺可核实授权；G-03 矩阵例外无 waiver；`GII-07` B 组门与协调文件及无关改动冲突；测试矩阵缺 `command::op_test` —— 均于 R7 修订行关闭 | 无 | R8 复审 |
| 2026-11-18 R8 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `f5c003050eacdc3871e70f7bab17fa5fbdb727b01bd13506e5691c21fa2ae8c1` | `PASS` | 无 P0/P1（事后核对：未识别 Codex R8 的 5 项 P1，已随 R8 修订行关闭） | P2×2（`GII-09` 证据锚点、`GII-22` AC1 措辞）：已修 | R9 复审 |
| 2026-11-18 R8 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `f5c003050eacdc3871e70f7bab17fa5fbdb727b01bd13506e5691c21fa2ae8c1` | `FAIL` | P1×5：网站提交可能混入他人已暂存改动；多网站文件未逐文件判定部署路径；`GII-07` 本卡特有文档语义判据未计数；`GII-07` 回滚无法独立撤销其两仓库交付；08:19:08 历史行把未核实的路径授权写成既成事实 —— 均于 R8 修订行关闭 | 无 | R9 复审 |
| 2026-11-18 R9 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `51af0fcc286f4232a2b45be5cdcef64bd9e37f43e485ddc9b763104b6533f90a` | `FAIL` | P1×1：受审字节仍以 `DEP-GII-02` 门控网站路径，与用户 2026-11-18 11:55:09 UTC 确认（网站检出已确认、不得再设确认前置）不符 —— 于 R9 修订行关闭；事后核对：未识别 Codex R9 的 4 项 P1，已随 R9 修订行关闭 | P2×2（补偿幂等检查不精确、第 2 步「不在 `cf` 即 STOP」与模板切换规则的差异未注明）：已修 | R10 复审 |
| 2026-11-18 R9 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `51af0fcc286f4232a2b45be5cdcef64bd9e37f43e485ddc9b763104b6533f90a` | `FAIL` | P1×4：`GII-07` 网站补偿先 `revert` 再走前置与评审，顺序不可执行；补偿假定网站提交必然存在；`GII-21` AC1/AC6 含两路径断言致计数低报；`GII-21` 用户可见行为变化却记文档 `N/A` —— 均于 R9 修订行关闭 | 无 | R10 复审 |
| 2026-11-18 R10 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `d70cff65f7c6d0fa59fa0645d6b15d35c76c88300f0b30eda4d300ce9fd88129` | `FAIL` | P1×1：`GII-14` AC4 映射含 AC 之外的 stderr 断言 —— 于 R10 修订行关闭；事后核对：未识别 Codex R10 的 3 项 P1，已随 R10 修订行关闭 | P2×1（`GII-02` AC3 映射的错误码检查可写明为执行条件）：保留为残余 | R11 复审 |
| 2026-11-18 R10 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `d70cff65f7c6d0fa59fa0645d6b15d35c76c88300f0b30eda4d300ce9fd88129` | `FAIL` | P1×3：网站补偿评审未暂存差异（应 `diff --staged`）；`revert -n` 后的补偿提交不含幂等标记；本仓库 `revert` 未带 `-s` 不产生 DCO —— 均于 R10 修订行关闭 | 无 | R11 复审 |
| 2026-11-18 R11 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `7921b12f8c2ce100c0a97ebaa6975ffc75a55e100e8874d8081bac0000bf13e6` | `PASS` | 无 P0/P1（事后核对：未识别 Codex R11 的 3 项 P1，已随 R11 修订行关闭） | P2×1（`GII-02` AC3 映射的错误码检查）：保留为残余 | R12 复审 |
| 2026-11-18 R11 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `7921b12f8c2ce100c0a97ebaa6975ffc75a55e100e8874d8081bac0000bf13e6` | `FAIL` | P1×3：补偿用 `revert -n` 会在暂存集合核对前覆盖无关改动；按标记跳过步骤会在中断后漏做推送与部署核验；撤回后的 `GII-07` 仍会随 `GII-08` 自动完成 —— 均于 R11 修订行关闭 | 无 | R12 复审 |
| 2026-11-18 R12 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `426c3770e8a1e6f56732ac34061a9251eebf70c9c3d747af434b2b26db43657d` | `FAIL` | P1×1：`FIX-GII-07` 在已无未完成 `independent` 卡时没有承载点 —— 于 R12 修订行关闭；事后核对：未识别 Codex R12 的 4 项 P1，已随 R12 修订行关闭 | P2×1（`GII-02` AC3 映射）：已修 | R13 复审 |
| 2026-11-18 R12 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `426c3770e8a1e6f56732ac34061a9251eebf70c9c3d747af434b2b26db43657d` | `FAIL` | P1×4：`FIX-GII-07` / `GII-07` / `GII-08` 依赖死锁；续作重跑第 2 步会拒绝本卡自己的改动与提交；`ls-remote` 输出与 `HEAD` 格式不同；保留 P2 无具名接受人 —— 均于 R12 修订行关闭 | 无 | R13 复审 |
| 2026-11-18 R13 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `bd8f9f8241c1e3e04350fec09ae41d43e7e8b07a9fe9af3bd477fcff060a8e1f` | `FAIL` | P1×1：`GII-07` 登记 `DEFER-*` 时承载卡仍要求两卡本地条件，延后路径阻塞 —— 于 R13 修订行关闭；事后核对：未识别 Codex R13 的 4 项 P1，已随 R13 修订行关闭 | 无 | R14 复审 |
| 2026-11-18 R13 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `bd8f9f8241c1e3e04350fec09ae41d43e7e8b07a9fe9af3bd477fcff060a8e1f` | `FAIL` | P1×4：补偿在原卡未产生网站提交时可能留下过期网站文档；删除场景与第 3/7 步 `M` 判据矛盾；`GII-07` 把 B 组强制门计入 VER；修订历史以上界充当事件时间 —— 均于 R13 修订行关闭 | 无 | R14 复审 |
| 2026-11-18 R14 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `8b906260c235f2e733c6f44feccb19bbd8a26768a5836246cea762d7eb01c237` | `PASS` | 无 P0/P1（事后核对：未识别 Codex R14 的 2 项 P1，已随 R14 修订行关闭） | 无 | R15 复审 |
| 2026-11-18 R14 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `8b906260c235f2e733c6f44feccb19bbd8a26768a5836246cea762d7eb01c237` | `FAIL` | P1×2：延后 `GII-07` 后 Phase 1 / 全计划退出条件仍要求其完成；删除补偿要求线上先含旧句，阻塞线上已是目标状态的恢复 —— 均于 R14 修订行关闭 | 无 | R15 复审 |
| 2026-11-18 R15 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `d7914c6f7d903a12c18bb4c70dec3d0c106d6756404c65cb6a547f5cb41bf290` | `PASS` | 无 P0/P1（事后核对：未识别 Codex R15 的 3 项 P1，已随 R15 修订行关闭） | 无 | R16 复审 |
| 2026-11-18 R15 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `d7914c6f7d903a12c18bb4c70dec3d0c106d6756404c65cb6a547f5cb41bf290` | `FAIL` | P1×3：延后 `GII-07` 后「完成判据」仍要求全部任务的 `Verification` 与全部卡完成才进全量门；网站第 3、7 步写快照文件违反 v2.12；`GII-16` 回滚测试只比计数 —— 均于 R15 修订行关闭（`GII-07` 延后路径随补偿改为 ER-10 前滚一并删除） | 无 | R16 复审 |
| 2026-11-18 R16 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `43b8843a866dfc2a98c7190833e9ce2337977218ecb3553b2e31ba98b0881618` | `FAIL` | P1×1：判定句 `M` / `W` 只排除 Markdown 标记，未排除会被 HTML 转义的字符，「渲染页面逐字包含」的声称对含 `'` / `&` 的句子不成立 —— 于 R16 修订行关闭；事后核对：未识别 Codex R16 的 3 项 P1，已随 R16 修订行关闭 | P2×1（`GII-07` 补偿应按第 1–7 步）：已修 | R17 复审 |
| 2026-11-18 R16 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `43b8843a866dfc2a98c7190833e9ce2337977218ecb3553b2e31ba98b0881618` | `FAIL` | P1×3：允许延后但串行链与 Phase 进入条件仍要求前驱完成，未规定下游依赖如何改写；`GII-16` 沿用的 `update_failure` 在写入前返回，测不出事务内部分写入；`GII-16` 要求一次回放补齐 ≥ 3 个 marker，与测试下每次回放一批（2 个）的既有分页冲突 —— 均于 R16 修订行关闭 | 无 | R17 复审 |
| 2026-11-18 R17 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `e1401a6dda9bfab44f5df4f71079d607f4e4c1a8b4c142ba2d9ea70a8b141e00` | `PASS` | 无 P0/P1（事后核对：未识别 Codex R17 的 16 项 P1，已随 R17 修订行关闭） | P2×2（`GII-14` 说明「index 零写入」、`GII-07` 脚本只判子串）：已修 | R18 复审 |
| 2026-11-18 R17 | Codex (`gpt-6.1-terra`, high；4 分区并行) | 全文分 4 区（A 非卡片章节、B `GII-01`..`GII-07`+`GII-19`、C `GII-08`..`GII-12`+`GII-20`/`22`、D `GII-13`..`GII-18`+`GII-21`），受审 SHA-256 `e1401a6dda9bfab44f5df4f71079d607f4e4c1a8b4c142ba2d9ea70a8b141e00` | `FAIL` | P1×16：A 网站验证重写未登记的生成文件；B `GII-02` 未证明使用显式工作树、`GII-03` 未测 `GIT_WORK_TREE` 与其优先级、`GII-05` 未证明 `!` 别名不被执行、`GII-01` 交错限制无验收、`GII-07` 脚本未判完整错误码标记；C `GII-10` diff 未逐字核对、`-vv` 未核已暂存 diff、HEAD 只比 OID，`GII-11` 循环夹具只判退出码，`GII-12` AC5 未核端点；D `GII-13` 可在无标记时空真通过、`GII-15` 未证明返回本地存储、`GII-18` 哈希断言未绑定 `verify_object` 且守卫只匹配字面、`GII-18` AC6 断言了不存在的返回值、`GII-21` AC8 只看 index —— 均于 R17 修订行关闭 | 无 | R18 复审 |
| 2026-11-18 R18 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `d0c2a2cc08b3166771dd3944cb3064b8d2f240e4c8f9a6f1f1cb5d2e9c48b4c4` | `FAIL` | P1×1：`GII-08` 说明承诺 Git 完整文案而 AC1 只判子串 —— 于 R18 修订行关闭；事后核对：未识别 Codex R18 的 15 项 P1，已随 R18 修订行关闭 | P2×1（`GII-15` AC2 生效行应标为执行条件）：保留，`GII-15` AC2 映射已写明「证明命中目标读取」 | R19 复审 |
| 2026-11-18 R18 | Codex (`gpt-6.1-terra`, high；4 分区并行) | 全文分 4 区，受审 SHA-256 `d0c2a2cc08b3166771dd3944cb3064b8d2f240e4c8f9a6f1f1cb5d2e9c48b4c4` | `FAIL` | P1×15：A 生成产物未入写集与协调、Phase 2 目标仍写 `ELOOP` 告警；B `GII-01`/`GII-19` 未测错误渲染路径、`GII-05` 未测展开后的错误渲染、`GII-07` 文档谓词与行数条件低报；C `GII-09`..`GII-22` 未覆盖无 schema 守卫路径、`GII-08` 未固定初始分支、`GII-22` 未测 `blake3`、`GII-12` 未证 checkout 切换与失败无副作用；D `GII-15` 时间戳早于释放、`GII-16` 未证 `add` 走批量路径、`GII-17` 未经共享入口、`GII-18` 结构守卫可绕过、`GII-21` AC8 空真 —— 均于 R18 修订行关闭 | 无 | R19 复审 |
| 2026-11-18 R19 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `3e1b17b7edac939f1f5bedf585f1b80cbf86225a505ccb59451656b3b7ae6017` | `PASS` | 无 P0/P1（事后核对：未识别 Codex R19 的 16 项 P1，已随 R19 修订行关闭） | P2×1（`GII-15` AC2 生效行标为执行条件）：已修 | R20 复审 |
| 2026-11-18 R19 | Codex (`gpt-6.1-terra`, high；4 分区并行) | 全文分 4 区，受审 SHA-256 `3e1b17b7edac939f1f5bedf585f1b80cbf86225a505ccb59451656b3b7ae6017` | `FAIL` | P1×16：A 生成文件缺失时第 4 步无法启动；B `GII-04` 未证其它命令走裸上下文、`GII-03` HEAD 只比 OID、`GII-05` AC3 未断言错误信封字段、`GII-06` 超长路径未验告警；C `GII-10` 期望由 Libra 输出构造、`GII-12` AC5 集合比较放过重复行；D `GII-14` `-f` 未验 index 不变、`GII-16` 与 `GII-13` 逐对象断言计数与本计划规则冲突、`GII-18` 损坏对象可走读取失败分支、`GII-21` AC8 只判完整性、`GII-23` 快照不含类型与权限且未验省略端点 —— 均于 R19 修订行关闭（`GII-13` / `GII-16` 计数经粒度规则与模板对齐后成立） | 无 | R20 复审 |
| 2026-11-18 R20 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `98d0b7b12ab208c0176dafe4b34715e9fde549b525a1ec33743e7834e6e0ba35` | `FAIL` | P1×1：`GII-17` AC1/AC2 的 `rg` 守卫只拒绝字面 `fn hermes_bounded_extraction` 与 `== "hermes"`，可被改名或 `.eq("hermes")` 绕过 —— 于 R20 修订行关闭；事后核对：未识别 Codex R20 的 11 项 P1，已随 R20 修订行关闭 | 无 | R21 复审 |
| 2026-11-18 R20 | Codex (`gpt-6.1-terra`, high；4 分区并行) | 全文分 4 区，受审 SHA-256 `98d0b7b12ab208c0176dafe4b34715e9fde549b525a1ec33743e7834e6e0ba35` | `FAIL` | P1×11：A `status.branch` 使逐文件短状态非空、页面检查在 `errexit` 下不安全；B `GII-04` AC6 `config list` 遮蔽加密值、`GII-02` AC3 只判非 `{` 开头、`GII-07` AC11 未要求失败；C `GII-12` AC5 期望来自 Libra 自身 `merge-base`、`GII-11` 无法观测「不经链接访问」、`GII-09` 既有校验归属错误、`GII-09` 未写明拆出 `GII-20`；D `GII-21` 未覆盖最终分量链接的跟随、AC8 未比较整个 `ScanResult` —— 均于 R20 修订行关闭 | 无 | R21 复审 |
| 2026-11-18 R21 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `38f5b313de11d9613b68aa88e7bec622216cfc3fb96fd49abda3cfe2ac17fa68` | `PASS` | 无 P0/P1（事后核对：未识别 Codex R21 的 7 项 P1，已随 R21 修订行关闭） | 无 | R22 复审 |
| 2026-11-18 R21 | Codex (`gpt-6.1-terra`, high；4 分区并行) | 全文分 4 区，受审 SHA-256 `38f5b313de11d9613b68aa88e7bec622216cfc3fb96fd49abda3cfe2ac17fa68` | `FAIL` | P1×7：A 页面检查在 `pipefail` 下命中可被判为 141；B `GII-06` 只比顶层条目名且未证仓库可用；C 失败类 AC 未要求非 0 退出、`GII-11` 精确短状态受 `status.branch` 影响；D `GII-16` 进程级计数含 operation snapshot、`GII-15` 未证释放后确实读取了仓库配置、`GII-17` 守卫可被转义字面量与改名函数绕过 —— 均于 R21 修订行关闭 | 无 | R22 复审 |
| 2026-11-18 R22 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `ec34f6da1317c0a91f92ec525be3db06b0a208d4b065b61f54925366b90d2cb4` | `PASS` | 无 P0/P1（事后核对：未识别 Codex R22 的 5 项 P1，已随 R22 修订行关闭） | P2×1（`GII-06` 映射顺序）：已修 | R23 复审 |
| 2026-11-18 R22 | Codex (`gpt-6.1-terra`, high；4 分区并行) | 全文分 4 区，受审 SHA-256 `ec34f6da1317c0a91f92ec525be3db06b0a208d4b065b61f54925366b90d2cb4` | `FAIL`（C 区 `PASS`） | P1×5：A 最终树全量门与证据记录自引用；B `GII-02` 未测空 `--work-tree=` 与缺值、`GII-06` AC6 合并两个触发条件；D `GII-15` 未证读取结果被使用、`GII-17` 去注释会吞掉字符串中的 `//` —— 均于 R22 修订行关闭 | 无 | R23 复审 |
| 2026-11-18 R23 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `a2053d76b627b2af5a4f5a625b7b2ab7c0206350f5efc7f850c8f06212a79de1` | `PASS` | 无 P0/P1（事后核对：未识别 Codex R23 的 3 项 P1，已随 R23 修订行关闭） | 无 | R24 复审 |
| 2026-11-18 R23 | Codex (`gpt-6.1-terra`, high；4 分区并行) | 全文分 4 区，受审 SHA-256 `a2053d76b627b2af5a4f5a625b7b2ab7c0206350f5efc7f850c8f06212a79de1` | `FAIL`（C 区 `PASS`） | P1×3：A 最终全量门未证明被测输入等于 `T`；B `GII-02` AC8 把两个标志的缺值合为一条、计数低报；D `GII-21` 未验证悬空链接的完整快照 —— 均于 R23 修订行关闭 | 无 | R24 复审 |
| 2026-11-18 R24 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `d07ed5f2660380d0e3d74c3d6083cb81aa7b7873150ae4090c5a63e28c078f7a` | `PASS` | 无 P0/P1（事后核对：未识别 Codex R24 的 5 项 P1，已随 R24 修订行关闭） | 无 | R25 复审 |
| 2026-11-18 R24 | Codex (`gpt-6.1-terra`, high；4 分区并行) | 全文分 4 区，受审 SHA-256 `d07ed5f2660380d0e3d74c3d6083cb81aa7b7873150ae4090c5a63e28c078f7a` | `FAIL` | P1×5：A 最终全量门未在前后核对 `HEAD` 仍为 `T`；B `GII-02` 未验空格分隔 `--work-tree` 的拒绝、`GII-24` 未验 `--work-tree` 以 `-` 开头的取值；C `GII-10` 未验工作区不变；D `GII-18` 未以可执行检查绑定就地哈希 —— 均于 R24 修订行关闭 | 无 | R25 复审 |
| 2026-11-18 R25 | Claude (Subagent `claude-opus-5-5`) | 全文，受审 SHA-256 `ccdb789c88487d235fc671fc049cc9d599055e4a3445516990e51d97be451f8c` | `PASS` | 无 P0/P1 | 无 | R26 仅差异确认 |
| 2026-11-18 R25 | Codex (`gpt-6.1-terra`, high；4 分区并行) | 全文分 4 区，受审 SHA-256 `ccdb789c88487d235fc671fc049cc9d599055e4a3445516990e51d97be451f8c` | `PASS`（A/B/C/D 全部 `PASS`） | 无 P0/P1 | P2×1（C 区：`GII-22` AC1 夹具依赖默认对象格式）：已于 R25 修订行修正 | R26 仅差异确认 |
| 2026-11-18 R26 | Claude (Subagent `claude-opus-5-5`) | 仅差异确认（相对 R25 受审字节 `ccdb789c8848…`），受审 SHA-256 `973afedc5a696064bd282f88b9041585d2342e7f959e51a55bb7fa1470085f44` | `PASS` | 无 P0/P1 | 无 | 计划可提交 |
| 2026-11-18 R26 | Codex (`gpt-6.1-terra`, high) | 仅差异确认（相对 R25 受审字节 `ccdb789c8848…`），受审 SHA-256 `973afedc5a696064bd282f88b9041585d2342e7f959e51a55bb7fa1470085f44` | `PASS` | 无 P0/P1 | 无 | 计划可提交 |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-GII-01 | `alias.!<shell>` 外部命令别名任意执行 | 需独立安全沙箱与确认策略 | 明确安全模型后另立计划 | `COMPATIBILITY.md` |

执行期若正式延后某张任务卡，须以一次 ER-03 计划修订（经计划文本双审）完成下列全部动作后才生效：① 在本表新增 `DEFER-GII-*` 行（延后内容、原因、重启条件、承接位置），并在追溯表注明对应 issue 仅部分闭环、不得关闭；② 逐一评估以该卡为前驱的下游关系——串行链后继、Phase 进入条件与承载关系：后继与该卡之间没有真实功能前置（只为写集串行）时，改为依赖该卡的前驱并同步「实施顺序」、各卡 `Dependencies`、`Granularity` 与审计表；存在真实功能前置时，后继随之延后（同样登记）或保持 `blocked`，不得自动越过；被延后卡若是承载卡，其承载的 `no-release` 卡须在同一修订中改指具名的新承载点或随之延后。修订生效前，该卡的下游保持 `pending`。Phase 退出条件、里程碑与「完成判据」中的「非延后」口径只排除按本条完成登记的卡。

## 完成判据

- [ ] 全量收口门须在**最终代码树**上执行，并记录被测提交 `T` 的 `libra rev-parse HEAD` 与 `libra rev-parse 'HEAD^{tree}'` 及结果；被测输入必须恰为 `T`：运行前与运行后各执行一次 `libra rev-parse HEAD`、`libra rev-parse 'HEAD^{tree}'` 与 `libra status --porcelain --untracked-files=all`，两次的 `HEAD` 与树都须等于记录的 `T` 及其树，且状态输出除本计划文件与 `docs/development/plan/plan-status.md` 外不得有任何已修改、已暂存或未跟踪条目（被忽略的构建产物不在其列），任一不满足即该次运行无效、须重跑；共享工作区存在他人改动或可能被他人切换而无法满足时，改在仓库外检出 `T` 的独立副本中运行，对该副本在运行前后执行同样的三项检查（状态输出须为空）；此后的提交若只记录收口证据——从 `T` 到该提交之间每个提交的 `libra show --name-only --format=format: <提交>` 都只含本计划文件与 `docs/development/plan/plan-status.md`——则被测产品文件未变，不触发重跑（证据提交不必记录自身的 `HEAD`）；除此之外的任何文件变化都须在新树上重跑，与是否发生修复无关。
- [ ] 计划文本提交门：Claude（Subagent）与 Codex 双审对同一受审字节（同一 SHA-256）均返回字面结论 `VERDICT: PASS` 且 P0/P1 全部关闭；网站交付另需独立 Claude Code `VERDICT: PASS`，两者互不替代。
- [ ] 网站交付门：每张触及用户可见行为的非延后卡均在本卡内按「文档与网站验收命令」完成 `/Volumes/Data/gitmono/libra-backend` 的最终字节同步检查与前端验证；变更的网站文件另取得独立 Claude Code `PASS`，随后精确暂存、签名+DCO 提交、核对提交文件集合与已审字节一致、推送 `cf` 并逐文件核验部署；未变更的网站文件记录「文档与网站验收命令」第 3 步的未变更文件证据（线上未含本卡行为时须经部署恢复路径命中）；计划文本提交门的 Claude 与 Codex 双审不替代该网站门，任一环节失败即 STOP 且不得标 `complete`。
- [ ] 所有任务卡满足粒度规则 `G-*`；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务 `Lifecycle=done` **且** `Acceptance=complete`（ER-04）；停在 `remote-pending` 或 `blocked` 的卡不得通过本门。
- [ ] 所有非延后任务的 `Verification` 命令已按「验证执行协议」运行，命令、列出条数、汇总行、退出码与 UTC 时间已记录在对应卡内。
- [ ] 「兼容与文档收口」各项已完成或给出 `N/A` 理由（docs / compat / error-code / test-index）。
- [ ] 每张卡的 `Rollback mode` 已实际验证或记录不可验证的原因。
- [ ] 全部非延后任务卡完成后运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿，且修复全量暴露的全部 Bug（ER-13）。
- [ ] Claude 与 Codex 最终评审结论均为 `PASS`，P0/P1 全部关闭。
- [ ] 每张非延后 `independent` 发布卡已执行 `patch + 1`、版本面集合一致、`Cargo.lock` 仅由工具链刷新、提交推送及 `gh release create` 完成并取得 D 组证据（ER-08）。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）；`plan-status.md` 已随每次状态推进同步；`plan-long.md` 为 `N/A`（由协调者统一维护）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
