# `libra ui` 真实数据接入计划（2026-11-20）

**模板版本:** `v2.16`（本计划未具名 `code-only`，按自身默认发布合同执行：每张实现卡 `patch + 1`、完整 C/D、ER-05 review 与 ER-13/ER-14 分层测试。）

**状态:** 设计计划（草案，R1/R2/R3 三轮双评 FAIL 后修订至第四版，待 R4 复审）。七张活动卡 `UID-01`..`UID-07` 全部 `pending` / 验收空；本文件只规划任务，不宣称任何数据已接入窗口。

**卡 ID 前缀说明:** 使用 `UID-`（UI Desktop）以区别于 `issues/479` 已占用的 `UI-01`..`UI-03`。

## 文档职责

本文解决 `libra ui` 桌面窗口只显示内嵌演示数据（26 个设计页、`sessions.json` 演示目录、`screens/history.rs` 的静态提交表）而无法回答"我的仓库现在是什么状态、Agent 到底做了什么"的问题。目标是交付一条**只读、离主线程、可失败、成本可测**的真实数据接入路径，并让六个数据表面（History、Sessions、Operations、Memory、Review/Investigate runs）与一个可用性状态面按卡序从演示数据切换到仓库事实。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收。

### 适用范围

- 命令表面：`libra ui`（无参数启动、全局 flag 前后皆可、`--help` 与错误退出码不变）。
- 数据表面：仓库上下文、提交历史与归属、Agent session/checkpoint（`capture.db`）、operation v2 journal、memory episode 投影、review/investigate run 目录。
- 代码表面：`src/gui/**`（新增 `src/gui/data/`）、必要的 internal/命令层读 API 提升（单一事实源），以及 `src/command/ui.rs` 的文档化行为说明（不新增 CLI flag）。
- 文档与发布：`docs/commands/ui.md` EN+zh、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、相邻网站 `../libra-backend` 的 `commands/ui.en.md`、版本面与 `gh` 发布。

### 非目标

- **窗口内写操作**：restore / trust / rebase / checkout / 导入 / 克隆等按钮仍只显示预览提示（`DEFER-UID-01`）。
- **实时流式刷新**：不做 watcher 或增量推送（`DEFER-UID-02`）。
- **多仓库标签页与 `libra ui <path>`**：一次只接入当前工作目录仓库（`DEFER-UID-03`）。
- **远端/云数据**：不读 Mega2 traces、Cloud 快照或网络（`DEFER-UID-04`）。
- **其余演示页**：本计划不接真实数据（`DEFER-UID-05`，页面清单见该条目）。
- **diff/hunk 内容视图**：只接提交元数据与拓扑（`DEFER-UID-06`）。
- **run 全文（findings/diff 内容）**：只显示摘要（`DEFER-UID-07`）。
- **Windows/Linux 桌面人工验收**：验收平台为 macOS（`DEFER-UID-08`）。
- **复活 `libra code` / Web UI**：GPUI 原生窗口是唯一交互外壳。

### 成功定义

- 在仓库内启动 `libra ui` 时，六个数据表面与可用性状态面显示仓库真实数据；仓库外启动时窗口正常打开并显示明确空状态，不报错退出。
- 所有仓库 I/O 在单一后台 worker 上执行，GPUI 主线程不阻塞；读取全程不写仓库（无 operation 行、无 schema 升级、无 writer boundary）。
- 失败路径按 ADR-UID-06 的分类矩阵呈现（源级与条目级分开），不 panic、不静默降级成演示数据。
- 每卡同步 EN+zh 命令文档、开发文档、`docs/development/commands/README.md`、兼容矩阵与网站页面；网站 `cf` typecheck/build、Claude Code PASS 与部署检查通过。
- 计划完成判据全部满足（见「完成判据」）。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。过期锚点只能作为历史线索。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `libra ui` 由进程入口在主线程识别并调用 `gui::run` | `src/main.rs:539`、`src/main.rs:646`、`src/cli.rs:1401` |
| 代码入口 | `gui::run` 定义（GPUI 主线程） | `src/gui/mod.rs:30` |
| 代码入口 | 调度器路径的 `ui` 永远返回 `LBR-UNSUPPORTED-001` | `src/command/ui.rs:42`、`src/cli.rs:4012` |
| 代码入口 | `ui` 的 preflight 为 `none()`、scope 为 `ReadOnly`、豁免自动升级检查 | `src/cli.rs:1884`、`src/cli.rs:2266`、`src/cli.rs:3918` |
| 数据/状态 | 窗口数据是内嵌演示目录，不读仓库 | `src/gui/model.rs:14`、`src/gui/view.rs:1` |
| 数据/状态 | 演示目录另有 `search` 与 `capabilities` 两个消费者 | `src/gui/screens/search.rs:56`、`src/gui/screens/capabilities.rs:14` |
| 数据/状态 | History 提交表是静态常量 `COMMITS` | `src/gui/screens/history.rs:20` |
| 数据/状态 | History 现有筛选按作者 `eli` 分类、图形按作者 `Codex` 与行号绘制 | `src/gui/screens/history.rs:183`、`src/gui/screens/history.rs:224`、`src/gui/screens/history.rs:483` |
| 数据/状态 | History 内还有演示文案 "acme-libra" / "Design preview" | `src/gui/screens/history.rs:95`、`src/gui/screens/history.rs:107` |
| 数据/状态 | 窗口状态只有 `Desktop` 的页面/选择/输入字段 | `src/gui/desktop.rs:145`、`src/gui/desktop.rs:55` |
| 数据/状态 | `RequestScope` 解析与 scope pin API | `src/internal/worktree_scope.rs:80`、`src/internal/worktree_scope.rs:226` |
| 数据/状态 | Agent session/checkpoint 目录已拆分到 `.libra/private/capture.db` | `src/command/maintenance.rs:3558`、`sql/migrations/2026111601_capture_database.sql:304` |
| 数据/状态 | capture 库只读打开 API、库路径与错误消息 | `src/internal/ai/capture/store.rs:93`、`:41`、`:98`、`:807` |
| 数据/状态 | capture 库会话/checkpoint/事件/归属表 | `sql/migrations/2026111601_capture_database.sql:304`、`:135`、`:57`、`:329` |
| 数据/状态 | `session_page_sql` 是命令层 `pub(super)` helper（生产路径仅 session.rs；checkpoint 的复用只在测试里） | `src/command/agent/session.rs:390`、`src/command/agent/checkpoint.rs:1826`、`:3291` |
| 数据/状态 | `mod session` 在 agent 模块内是私有的 | `src/command/agent/mod.rs:48` |
| 数据/状态 | 会话↔提交链接 helper 接收注入连接，但内部仍会 `open_cached_repository` | `src/command/agent/session.rs:763`、`:799` |
| 数据/状态 | 文件会话存储 `SessionStore` 会在 `load()` 时迁移旧格式（有副作用） | `src/internal/ai/session/store.rs:29`、`:377` |
| 数据/状态 | memory episode 读面：`list_episodes` 无 limit，子表按 repo 全量取，freshness 用全源指纹 | `src/internal/ai/memory/reader.rs:198`、`:315`、`:165` |
| 数据/状态 | memory 全源指纹由投影写入 | `src/internal/ai/memory/projection.rs:183` |
| 数据/状态 | operation v2 存储 API；head 按 scope 记录 | `src/internal/operation/store.rs:330`、`:949` |
| 数据/状态 | operation 历史查询是命令层私有 helper：ranked CTE + COUNT + 外层 OFFSET，按可变 `end_ts` 排序 | `src/command/op.rs:642`、`:651`、`:662`、`:693` |
| 数据/状态 | CLI 分页从第 1 页开始 | `src/command/op.rs:251` |
| 数据/状态 | review/investigate run 落盘目录 `.libra/sessions/agent-runs/<run_id>/` | `src/internal/ai/review/mod.rs:6` |
| 数据/状态 | run 列表 API 会跳过不可解析项；逐项读取用无上限 `fs::read` | `src/internal/ai/review/store.rs:882`、`:1226`、`src/internal/ai/investigate/store.rs:935` |
| 数据/状态 | 屏幕静态 run 字面量 | `src/gui/screens/reviews.rs:31`、`src/gui/screens/investigate.rs:29` |
| 数据库连接 | `get_db_conn_instance()` 失败即 panic | `src/internal/db.rs:608` |
| 数据库连接 | 显式路径连接默认允许 schema 升级 | `src/internal/db.rs:624`、`:635` |
| 数据库连接 | 无 schema 管理连接（可传 busy timeout；仍是读写打开） | `src/internal/db.rs:217`、`:274` |
| 数据库连接 | `open_database_without_migrations`（跳过迁移；也是读写打开） | `src/internal/db.rs:874` |
| 数据库连接 | 连接失败被包装为 `io::Error::other` | `src/internal/db.rs:274`、`src/internal/ai/capture/store.rs:807` |
| 数据库连接 | 按连接检查 schema 的 API；按角色的 API 拒绝 Capture 角色 | `src/internal/db/schema.rs:381`、`:871`、`:877` |
| 数据库连接 | CLI 对只读命令关闭升级的入口（窗口路径绕过） | `src/cli.rs:3777` |
| 仓库定位 | `storage_path()`/`working_dir()` 在仓库外 panic；`try_get_storage_path` 返回错误 | `src/utils/util.rs:1262`、`:1269`、`:1464` |
| 用户命令 | `libra ui` 不读取也不写入仓库 | `docs/commands/ui.md:47` |
| 文档 | 开发文档把「接真实数据」列为未实现项 | `docs/development/commands/ui.md:37` |
| 文档 | 开发文档索引仍写 "Needs no repository" | `docs/development/commands/README.md:85` |
| 测试 | 现有 `ui_test` 只覆盖 help/多余参数/Linux 无显示 | `tests/command/ui_test.rs:1` |
| 测试 | session 列表分页契约测试 | `tests/agent_checkpoint_reader_test.rs:449` |
| 外部参照 | capture 库拆分已完成（CQ-04 `done`/`complete`） | `plan-status.md` 的 capture-queue 行 |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-UID-01 | 窗口无仓库上下文与数据接缝，只有静态演示数据 | 用户看不到自己的仓库/Agent 事实 | `src/gui/model.rs:14`、`src/gui/screens/history.rs:20` | `UID-01` |
| GAP-UID-02 | History 显示 9 条固定提交，筛选/图形基于作者名与行号，另有演示文案 | 历史、归属与文案错误 | `src/gui/screens/history.rs:20`、`:224`、`:95` | `UID-02` |
| GAP-UID-03 | Sessions 显示 8 条原型 session | 无法查看真实 Agent 会话与证据 | `src/gui/model.rs:14` | `UID-03` |
| GAP-UID-04 | Operations 页面是静态卡片 | 无法查看操作日志 | `src/gui/screens/operations.rs` | `UID-04` |
| GAP-UID-05 | Memory 页面是静态卡片 | 无法查看 episode 投影 | `src/gui/screens/memory.rs` | `UID-05` |
| GAP-UID-06 | Review/Investigate 运行列表是静态卡片 | 无法查看真实 run 状态 | `src/gui/screens/reviews.rs:31`、`src/gui/screens/investigate.rs:29` | `UID-06` |
| GAP-UID-07 | Splash/Welcome/About/Capability 是插图或固定文案 | 无法判断仓库与数据面可用性 | `src/gui/screens/splash.rs:67`、`src/gui/screens/about.rs:69`、`src/gui/screens/capabilities.rs:14` | `UID-07` |
| GAP-UID-08 | 主线程直接渲染，无 I/O 线程模型；`storage_path()`/`get_db_conn_instance()` 会 panic | 真实读取会冻结或崩溃窗口 | `src/utils/util.rs:1269`、`src/internal/db.rs:608`、`src/gui/mod.rs:30` | `UID-01` |
| GAP-UID-09 | 文档仍写「不读取仓库」「演示数据」 | 文档与行为不一致 | `docs/commands/ui.md:47`、`docs/development/commands/ui.md:37` | `UID-01`..`UID-07` |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| [`plan-long.md`](plan-long.md) | 不新增 LR；RT-01 只保留历史封存 | 不复活 `libra code`/Web UI；只在该文件的日期计划索引登记本计划 |
| [`plan-20261116-capture-queue.md`](plan-20261116-capture-queue.md) | 前置（incoming）：`capture.db` 拆分与只读打开 | 消费 `.libra/private/capture.db` 布局（`DEP-UID-01`） |
| [`plan-20260926.md`](plan-20260926.md) | 前置（incoming）：memory episode 只读投影 API | 只消费 `memory` 读面（`DEP-UID-02`） |
| `plan-20261117-*.md`（15 份 Git 兼容计划） | 写集协调（incoming）：`COMPATIBILITY.md`、`docs/commands/`、`docs/development/commands/README.md`、网站 `commands/` 页面 | 按 ER-12 串行窗口写入（`DEP-UID-03`） |
| `../libra-backend` | 网站文档交付（incoming） | 每卡同步 `commands/ui.en.md`、`cf` typecheck/build、Claude Code PASS 与部署检查（`DEP-UID-04`） |
| 本次 `libra ui` 全局 flag 修复 | 基线前置（incoming） | 开工前确认 main 上的 `ui_window_launch` 使用 clap 解析（`DEP-UID-05`） |
| `plan-status.md` | 协调者共享写集 | 每卡推进必须在同一变更更新对应行与 §五/§六 登记 |
| `docs/development/commands/ui.md` | 命令开发事实源 | 更新其「当前状态 / 还未实现的功能」章节 |
| [`docs/development/internal/stats.md`](../internal/stats.md) | 统计面事实源 | 只登记 UI 消费哪些统计字段，不改统计口径 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 值得做：窗口已是默认构建的一部分，但只显示演示数据 | 无 |
| 可行性 | 可拆：接缝卡 + 六个表面卡，每卡单轴、M 规模 | 按表面拆成 7 卡 |
| 任务卡粒度 | 三轮发现 AC 捆绑、计数口径、测试路径不可选 | AC 单一谓词 + 具名 test fn（`gui::data::tests::*`）；矩阵移入 ADR |
| 依赖与顺序 | 发现写集相交未入图、DEP 粒度/消费者错误 | 补齐串行链与文件级写集窗口；修正消费者 |
| 完整性 | 补齐共享读 API 提升写集（含 `agent/mod.rs`）、`T-1` 门、D 组 job 清单 | 见各卡与 ER-04 |
| 安全性 | 只读面、fail-closed、redaction 复用、scope pin、错误保留与脱敏 | 见 GC-UID-02/04 与 ADR-UID-06 |
| 功能正确性 | 源级/条目级状态分离；History 归属与父链真实化；分页 revision token | 见 ADR-UID-06 与各卡 AC |
| 接口兼容 | 不新增 CLI flag、不改退出码；提升 API 保持 CLI 输出 | 见各卡 Verification |
| 数据流与控制流 | worker deadline + 取消标志 + scope pin + 有界读取 | 见 ADR-UID-01 |
| 性能与容量 | 按既有 API 真实成本写契约（不做虚假 O(200) 承诺；freshness 保留全源成本） | 见「性能与容量摘要」 |
| 可靠性与容错 | 损坏 run 逐项隔离；worker 停滞有预算与归属 | 见故障恢复矩阵 |
| 可维护性 | 共享读 API 提升为单一事实源 | 见 GC-UID-03 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-20 17:10:00 UTC | 自审（成稿） | 首版：6 卡、5 项 ADR、5 项依赖、串行发布 | N/A | 实施顺序、依赖登记表、粒度审计表 |
| 2026-11-20 18:05:00 UTC | Codex R1 FAIL + Claude R1 FAIL | 拆 Operations/Memory；卡 ID 改 `UID-*`；AC 逐谓词；补串行依赖边；补共享读 API 提升与 `T-1`；D 组补 `release.yml`；rollback 改 `compensating`；DEP 补证据与失败路径；修正锚点与计数；补守卫命令；补 §五/§六/plan-long 登记 | `UI-01..UI-06` → `UID-01..UID-07` | 实施顺序、依赖登记表、粒度审计表、测试矩阵、追溯表、里程碑、风险表 |
| 2026-11-20 19:10:00 UTC | Codex R2 FAIL + Claude R2 FAIL | 注入连接 + `open_connection_without_schema_management(busy_timeout)`；scope pin；memory 伴生有界 API（不改原签名）；op 真实成本 + head pin；run 目录/字节上限；源级/条目级状态分离；D 组枚举实际 job；守卫改具体模式；`op log`、相对路径、prod-file 计数、DEP 粒度与等待上限；补偿演练收口一次 | `UID-04`/`UID-05` 口径调整 | 各卡 AC/VER/写集、ADR、ER-04/ER-06a、依赖登记表、粒度审计表、性能摘要、故障矩阵 |
| 2026-11-20 20:05:00 UTC | Codex R3 FAIL + Claude R3 FAIL | 归属双连接注入并移除内部 `open_cached_repository`；补 `agent/mod.rs` 写集与计数；关闭 ADR/AC 对齐（预算内结束或记录 stalled，窗口不阻塞）；memory freshness 保留全源成本、只做页内行/子表有界；op 分页改 revision token（全 scope head 集 + 行数 + max `end_ts`，同事务读取，页从 1 起）；run 改为打开句柄的有界读 + 全目录扫描后取最新 100（扫描上限 5000 + truncated 指示）；schema 检查用按连接 API，Capture 角色读自身 `schema_versions`；权限错误前置探测保留 `io::ErrorKind` 并脱敏；AC 重新拆分并给具名 test fn；守卫正则 `\b(rv|iv)-[0-9a-f]{6}\b`；修正 DEP 消费者、README 写集窗口、DEFER 页面数；补回「非目标与延后项」「完成判据」章节 | `UID-02`/`UID-03` 计数与写集调整 | 各卡 AC/VER/写集、ADR-UID-01/03/06、依赖登记表、粒度审计表、性能摘要、故障矩阵、§五/§六 |

## 已决议设计决策

### ADR-UID-01: 窗口数据经单一后台 worker 读取，主线程只渲染快照

- **Status:** Accepted
- **Context:** GPUI 必须占用进程主线程（`src/gui/mod.rs:30`）；读取涉及 SQLite、对象库与文件系统；`storage_path()`/`working_dir()` 在仓库外 panic（`src/utils/util.rs:1269`、`:1464`）；既有读面多为 async API。
- **Decision:** `src/gui/data/` 提供一个常驻 `std::thread` worker：线程内建立**一个长期存在的 current-thread Tokio runtime** 执行 async 读 API，并用 `WorktreeScope::pin_request_scope`（`src/internal/worktree_scope.rs:226`）固定仓库 scope。连接由调用方注入：仓库库用 `open_connection_without_schema_management(path, busy_timeout ≤2s)`（`src/internal/db.rs:217`），capture 库用 `open_readonly`（`src/internal/ai/capture/store.rs:93`）；worker 不调用 `get_db_conn_instance()`。每次请求带 deadline（busy timeout + 行/字节上限）；worker 在请求边界检查取消标志。窗口关闭时置取消标志，并在关闭预算 3s 内等待；若某次阻塞读仍超出 deadline，worker 记录 `worker stalled` 状态后结束，**surviving 线程由 worker handle 持有**（不 detach；进程退出时由 OS 回收），窗口在预算内返回、不阻塞主线程。
- **Alternatives considered:** GPUI executor 上做 I/O（仍受主线程调度）；每次操作新建 runtime（启动成本与线程泄漏）。
- **Consequences:** 引入 supersession、取消、scope pin 与 stalled 归属语义；禁止主线程直接调用 `storage_path()`/`get_db_conn_instance()`；数据层测试必须无显示器且能注入慢读。
- **Revisit when:** GPUI 提供稳定的后台 executor，或需要流式刷新（`DEFER-UID-02`）。

### ADR-UID-02: 只读边界——无 schema 管理、无 writer boundary、无 operation 行

- **Status:** Accepted
- **Context:** 显式路径连接默认允许 schema 升级（`src/internal/db.rs:624`）；CLI 对只读命令用 `set_schema_upgrade_allowed(false)`（`src/cli.rs:3777`），而窗口路径绕过 CLI preflight。
- **Decision:** 窗口读取使用跳过 schema 管理的连接（`open_connection_without_schema_management`，可传 busy timeout）与 capture 库 `open_readonly`；这些连接仍以读写方式打开，但调用面**从不写**（不获取 writer boundary/lease、不写 operation journal、不触发迁移），只读性由调用面与判据保证。schema 落后/超前按 ADR-UID-06 分类显示，不自动迁移。
- **Alternatives considered:** 复用 CLI preflight 全流程（会把迁移与锁语义带进窗口）；允许自动升级（只读命令改变磁盘状态）。
- **Consequences:** 每卡必须提供"读后仓库状态不变"判据；升级动作留给 CLI。
- **Revisit when:** 窗口需要写操作（`DEFER-UID-01`）。

### ADR-UID-03: 单一事实源——共享读 API 提升，GUI 只做投影

- **Status:** Accepted
- **Context:** 需要复用的读逻辑部分是命令层私有 helper：`session_page_sql`（`src/command/agent/session.rs:390`，`pub(super)`）、会话↔提交链接（`:763`，内部仍会自开连接）、operation 历史查询（`src/command/op.rs:662`）、memory 读面（`src/internal/ai/memory/reader.rs:198`）。GUI 若自行复制会漂移（GC-02）。
- **Decision:** 只做**可见性提升 + 连接注入 + 父模块导出**，不改变 SQL/语义：`session_page_sql` 与取行函数提升并在 `src/command/agent/mod.rs` 导出；`read_session_commit_links` 改为**双连接注入**（repo + capture），移除内部 `open_cached_repository`；`query_operation_log_page` 提升为 `pub(crate)`；memory 新增**伴生**有界读 API（保留 `list_episodes` 原签名）。GUI 只把结果转换为行类型。唯一允许 GUI 直接反序列化的是 run 的公开 wire 类型（`ReviewRunState`/`ReviewManifest` 等）——这是复用 wire 类型而非复制业务逻辑，用于实现有界读。
- **Alternatives considered:** GUI 解析命令 stdout（需要 OutputConfig、易漂移）；GUI 复制 SQL（违反 GC-02）。
- **Consequences:** 提升卡必须证明 CLI 行为未变（既有命令测试），并按 ER-13 `T-1` 跑全量。
- **Revisit when:** 某表面需要 CLI 独有输出布局。

### ADR-UID-04: 隐私与路径卫生

- **Status:** Accepted
- **Context:** Agent 会话内容可能含 secret 与主机绝对路径；ER-11 要求 sanitized 证据，窗口同样是用户可见面。
- **Decision:** 显示前复用既有 redaction（`utils::redact` / `observed_agents::redaction`）；所有路径在快照边界转换为仓库相对路径（`session_page_sql` 的绝对 `working_dir` 与 capture 打开错误消息里的绝对路径都必须转换/脱敏）；快照不含原始 tool payload 全文，单条截断 ≤4 KiB。
- **Alternatives considered:** 原样展示（泄漏风险）；GUI 内另写 redaction（重复实现）。
- **Consequences:** 部分内容被掩码；截断上限写入性能预算。
- **Revisit when:** 远端/云数据进入窗口（`DEFER-UID-04`）。

### ADR-UID-05: 仓库可选，窗口永不因缺少仓库而失败

- **Status:** Accepted
- **Context:** `libra ui` 可在任意目录启动；改为"必须有仓库"会破坏现有退出码与用户预期；`storage_path()`/`working_dir()` 在仓库外 panic。
- **Decision:** 仓库定位只用 `RequestScope::try_resolve`（`src/internal/worktree_scope.rs:80`）与 `try_get_storage_path`（`src/utils/util.rs:1262`），不使用会 panic 的包装函数；找到仓库则加载数据，找不到则以空上下文启动。
- **Alternatives considered:** 无仓库即退出（破坏现有行为）；新增 `--repo` 参数（推迟到 `DEFER-UID-03`）。
- **Consequences:** 每个表面必须实现空状态；空状态不得伪装成演示数据。
- **Revisit when:** 引入 `libra ui <path>` 或多仓标签页。

### ADR-UID-06: 源级与条目级状态分类

- **Status:** Accepted
- **Context:** R1/R2/R3 发现同一缺失数据源被同时定义为错误和空状态；源级损坏与条目级损坏恢复语义不同；现有 open API 会把连接失败包装为 `io::Error::other`，`capture::open_readonly` 用 `is_file()` 会把不可访问父目录当作 NotFound。
- **Decision:** 分类函数只返回以下状态，GUI 按返回值渲染：

  - **源级**：`absent`（未创建 → 空状态）、`unreadable`（保留 `io::ErrorKind`，含 PermissionDenied → 类型化错误）、`corrupt`（库结构损坏 → 类型化错误）、`schema-old`（库版本低于二进制 → "用 CLI 升级"）、`schema-new`（库版本高于二进制 → "二进制过旧"）、`stale`（投影过期 → 提示 `libra memory rebuild`）。
  - **条目级**：`ok`、`malformed`（内容不可解析 → 该行不可读）、`oversized`（超过字节上限 → 该行标记超大）、`incomplete`（产物缺件 → 该行提示不完整）。

  源级判定必须做**前置探测**：先 `std::fs::metadata` 保留 `NotFound`/`PermissionDenied`/其它 io kind，再打开；capture 打开错误消息在渲染前脱敏（去掉绝对路径）。schema 判定用按连接 API `inspect_schema_for_connection(conn, role)`（`src/internal/db/schema.rs:381`）；Capture 角色不被按角色 API 接受（`:877`），因此 capture 库的 `schema-old`/`schema-new` 由注入的 capture 连接读取其 `schema_versions` 判定。页面级状态不得自定义：过期映射到 `stale`，产物缺件映射到 `incomplete`。
- **Alternatives considered:** 各页自定义语义（验收冲突）；一律报错（空仓库体验差）。
- **Consequences:** 每卡 AC 只断言"分类函数返回并可渲染"，矩阵细节在本 ADR 而非 AC 中计数。
- **Revisit when:** 引入远端数据源（网络不可达成为第七类源级状态，见 `DEFER-UID-04`）。

## 全局工程约束

- **GC-01 现状核实前置:** 每个任务开工前重新核对计划、相关开发文档、用户文档、当前代码和测试。
- **GC-02 单一事实源:** 状态机、schema、输出结构、权限策略、配置解析和共享 helper 必须有单一事实源。
- **GC-03 Git 互操作与 Libra 扩展边界:** Git 兼容表面必须说明与 Git 的一致点和有意差异；Libra-only 表面必须说明替代工作流、用户影响和机器接口。
- **GC-04 输出与错误契约:** 用户可见错误使用稳定 `LBR-*` 码并同步 `docs/error-codes.md`。
- **GC-05 文档同步:** 命令或公开行为变化必须同步 `docs/commands/<cmd>.md`、zh-CN、开发文档、`COMPATIBILITY.md` 和测试索引。
- **GC-06 测试索引:** 新增或重命名 `--test` target 必须更新 `tests/INDEX.md`。
- **GC-07 安全默认值:** 未满足前置时默认 fail-closed。
- **GC-08 原子性与恢复:** 必须定义事务边界、幂等键、崩溃窗口和回滚/前滚策略。
- **GC-09 并发与资源生命周期:** 锁、CAS、lease、队列、子进程、连接池、临时目录必须有释放/恢复语义。
- **GC-10 性能预算:** 不得引入无界扫描、无界内存或 N+1 调用；写出数据规模与断言。
- **GC-11 生产 panic 禁止:** 生产路径不得新增裸 `unwrap()`、`expect()`、`panic!()`。
- **GC-12 精确暂存:** 提交前只 `libra add <相关路径>`，不得使用 `commit -a`。
- **GC-13 数据库迁移作用域与全局配置隔离:** 读路径按数据库角色区分并显式登记 allowlist：`Repository` = `.libra/libra.db`（只读、不迁移）、`Capture` = `.libra/private/capture.db`（只读打开）、`GlobalConfig`/`SystemConfig` = 配置库（只经既有只读配置读取）。每连接声明角色与迁移策略；仓库库的 schema 判定不得施加到配置库；测试使用隔离的 `LIBRA_CONFIG_GLOBAL_DB`/`LIBRA_CONFIG_SYSTEM_DB`。

**计划专属约束:**

- **GC-UID-01 主线程零 I/O:** 所有读取经 `src/gui/data/` 的 worker（ADR-UID-01）。
- **GC-UID-02 只读不写:** 不写 operation 行、不获取 writer boundary、不推进 schema 版本（ADR-UID-02）。
- **GC-UID-03 不复制业务逻辑:** 复用既有或本卡提升的共享实现；唯一例外是 run 的公开 wire 类型反序列化（ADR-UID-03）。
- **GC-UID-04 路径与隐私卫生:** 快照不含宿主绝对路径；redaction 复用；payload 截断（ADR-UID-04）。
- **GC-UID-05 无演示兜底:** 不得回退到内嵌演示数据；`sessions.json` 与 `COMMITS` 常量在对应卡完成后删除。**过渡期例外**：`UID-03` 完成后 `model::catalog()` 仅保留给 `search`/`capabilities`（`DEFER-UID-05` 页面），Sessions 不得再调用；`UID-07` 移除该过渡接口。
- **GC-UID-06 无显示器可测:** 数据层测试放在 `src/gui/data/tests.rs`（生产文件不含测试代码），路径为 `gui::data::tests::*`；屏幕模块可保留内联测试，其 panic 守卫不适用。窗口人工验收需要 macOS 桌面会话。
- **GC-UID-07 文档同卡交付:** EN+zh、开发文档、`docs/development/commands/README.md`、兼容矩阵与网站页面同卡交付（ER-06a）。
- **GC-UID-08 状态矩阵统一:** 所有表面按 ADR-UID-06 返回状态。

## 执行检查必备需求（强制）

1. **ER-01 开工前安全检查:** `libra status --short --branch`，确认分支与工作区状态；默认在 `main` 上按卡直推。
2. **ER-02 先核对后实现:** 刷新源码/文档锚点、测试 target 与外部参照。
3. **ER-03 粒度门禁:** 按 `G-*` 复核本卡 `Granularity` 行；范围扩大先拆卡。
4. **ER-04 每卡验收门:** A 组（本卡 `Verification` 的 nextest 命令）+ B 组（`Task type`）+ C 组（发布收口）+ D 组（远端后置）累加。C 组按 ①版本面 parity → ②`patch + 1` 与三处版本面同步 → ③`cargo build` 由工具链刷新 `Cargo.lock` → ④bump 后重跑 fmt + 严格 Clippy + 本卡 A 组（`Full-suite trigger` 命中时改跑双环境全量 nextest）→ ⑤`cargo build --release` → ⑥安装 → ⑦`libra add <相关路径>` + `libra commit -s` → ⑧推送 `main` → ⑨`gh release create v<version>`。D 组逐 job 登记并取证：`base.yml`（push `main`）的 `format`/`clippy`/`redundancy`/`test`；`codeql.yml`（push `main`）的 `analyze`；`release.yml`（push `v*` tag）的 `build-and-upload`（四平台 leg）/`upload-install-scripts`/`update-homebrew-tap`/`verify-homebrew-formula`/`request-stable-manifest`，head SHA 为本卡提交；产物判据为 `download.libra.tools` 对应版本 HTTP 200 + 版本号匹配。任一失败前滚修复，不回退已发布 artifact。
5. **ER-05 review 闭环:** 代码 review 者为 Codex（维护者可指定 Claude），P0/P1 关闭并以 `VERDICT: PASS` 收口；网站 `cf` 推送前另需后端 `AGENTS.md` 要求的 Claude Code PASS。
6. **ER-06 文档与兼容同步:** 按 ER-06a 逐文件交付；网站写入前确认 `../libra-backend` 是 Libra 仓库且为 `cf`，跑 sync check、typecheck、build 与 Claude Code PASS；部署判据为公开页 URL HTTP 200 且新文案可见。
7. **ER-07 Libra-native 工作流与提交签名:** 只用 `libra status`/`libra add`/`libra commit -s`/`libra push origin main`；提交后校验 `gpgsig` 与 `Signed-off-by` 同一提交。
8. **ER-08 版本与发布:** 每卡 `patch + 1`，三处版本面同步，`Cargo.lock` 只由工具链刷新，`gh release create v<version>`。
9. **ER-09 push 失败策略:** 非 fast-forward 先 pull/merge 后重新验收再推。
10. **ER-10 内部服务错误:** 有界重试（≤5 次、≤30 分钟），超预算置 `blocked`。
11. **ER-11 证据卫生:** 证据只写摘要与计数。
12. **ER-12 并发边界与串行发布:** 全串行；单一发布者。
13. **ER-13 测试执行分层:** 执行阶段按 `Verification`；`Full-suite trigger` 按卡判定；全部卡完成后跑全量收口门并修复暴露的 Bug。
14. **ER-14 发布集成测试执行器:** focused 证据以 nextest 执行；全量收口门用双环境 nextest；`cargo test --all` 只作诊断。

## 实施顺序

- `UID-01 -> UID-02`
- `UID-02 -> UID-03`
- `UID-03 -> UID-04`
- `UID-04 -> UID-05`
- `UID-05 -> UID-06`
- `UID-06 -> UID-07`

依赖边同时承载 G-10 的写集串行理由：七卡共享 `src/gui/desktop.rs`、`src/gui/data/mod.rs` 与同一组文档。

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-UID-01 | incoming | 跨计划 | [`plan-20261116-capture-queue.md`](plan-20261116-capture-queue.md)（CQ-04） | capture-queue 计划 owner | `.libra/private/capture.db` 承载 `agent_session`/`agent_checkpoint`/`agent_session_commit`，且存在只读打开 API；CQ-04 `done`/`complete` | `src/internal/ai/capture/store.rs:93`、`sql/migrations/2026111601_capture_database.sql:135`、`:329`、`plan-status.md` 的 capture-queue 行（核对 2026-11-20） | 布局变化时 `UID-02`/`UID-03`/`UID-07` 置 `blocked` 并登记 `FIX-*`；等待上限 1 个发布切片 |
| DEP-UID-02 | incoming | 跨计划 | [`plan-20260926.md`](plan-20260926.md)（DM-11 读面） | memory 计划 owner | `memory` 读面 API 在 main 可用（DM-11 已合入 main，卡面 `locally-accepted`）；本计划只要求 API 存在 | `src/internal/ai/memory/reader.rs:198`、`plan-status.md` 的 0926 行（核对 2026-11-20） | API 变更时 `UID-05`/`UID-07` 置 `blocked`；等待上限 1 个发布切片 |
| DEP-UID-03 | incoming | 跨计划（文件级写集窗口） | `plan-20261117-*.md` 15 份计划中当前写 `COMPATIBILITY.md`、`docs/commands/` 或 `docs/development/commands/README.md` 的活动卡（例如 `plan-20261117-branch.md#BR-01`、`plan-20261117-global-init-infra.md#GII-01`、`plan-20261117-plumbing-misc.md#PM-01`） | 20261117 批次发布者 | 文件级窗口：开工前扫描这些计划的 `in-progress` 卡，确认无卡写 `COMPATIBILITY.md`、`docs/commands/ui.md` 或 `docs/development/commands/README.md`（整文件，不论行）；窗口上限 1 个发布切片 | `plan-status.md` 20261117 批次节（核对 2026-11-20） | 窗口不可得则排队；超过 1 个发布切片置 `blocked` 并报告维护者 |
| DEP-UID-04 | incoming | 外部仓库 | `../libra-backend`（`cf`） | 网站维护者/本计划发布者 | `libra status --short --branch` 显示 `cf`；sync check、typecheck/build、Claude Code PASS 通过 | 后端 `AGENTS.md`「Libra command documentation delivery」（核对 2026-11-20） | 文档未交付 → `blocked`；已提交推送仅部署待确认 → `remote-pending`；等待上限 1 个发布切片 |
| DEP-UID-05 | incoming | 基线前置 | 本次 `libra ui` 全局 flag 修复提交 | 本计划发布者 | main 上 `ui_window_launch` 使用 clap 解析（`Cli::try_parse_from`），且 `ui_test` 通过 | `src/cli.rs:1401`、`tests/command/ui_test.rs`（核对 2026-11-20） | 未合入则 `UID-01` 置 `blocked`；等待上限 1 个发布切片；不得在本计划内夹带该修复 |

### 发布分组与并发窗口

默认每张卡独立发布（G-07）；本计划不登记 `REL-*`。

**并发声明:** 全串行（七卡写集相交于 `src/gui/desktop.rs`、`src/gui/data/mod.rs`、`docs/commands/ui.md`、`COMPATIBILITY.md`、`plan-status.md`；G-10 I–I 相交禁止并发）。

**发布者:** 单一发布者（维护者指定；未指定时由执行 Agent 串行承担 bump/构建/提交/推送/`gh release create` 与 D 组取证）。

**发布窗口顺序:** `UID-01` → `UID-02` → `UID-03` → `UID-04` → `UID-05` → `UID-06` → `UID-07`；同一时刻只允许一张卡处于「已 bump 未完成推送 / 未触发 release」状态。

### Phase 0: 基线冻结和消歧

**目标:** 冻结 `libra ui` 行为与数据面事实，确认七个表面的数据来源与既有/待提升 API，冻结写集。

**进入条件:** `libra status --short --branch` 干净；`DEP-UID-01`/`DEP-UID-02`/`DEP-UID-05` 满足；`UID-01` 粒度行通过 ER-03。

**退出条件:** 锚点在开工日复核；维护者批准计划文本（R4 review `PASS`）并授权 `UID-01`。

### Phase 1: 只读数据接缝

**目标:** 交付 `src/gui/data/` worker 接缝、仓库上下文解析（复用 `RequestScope`）、源级/条目级状态分类、快照/错误类型与生命周期管理。

**进入条件:** Phase 0 退出条件满足。

**退出条件:** `UID-01` `done`/`complete`；窗口在仓库内/外都能启动并显示上下文或空状态，主线程零 I/O 与只读性有测试证据。

### Phase 2: 六个数据表面

**目标:** 按卡序把 History、Sessions、Operations、Memory、Review runs、可用性状态六个表面切到真实数据。

**进入条件:** `UID-01` `done`/`complete`。

**退出条件:** `UID-02`..`UID-07` 全部 `done`/`complete`；`sessions.json`、`COMMITS` 常量与静态 run 字面量已删除；`model::catalog()` 仅保留给 `DEFER-UID-05` 页面（Sessions 不再调用），并由 `UID-07` 移除或标注过渡。

### Phase 3: 收口

**目标:** 全量收口门、文档一致性、计划状态同步。

**进入条件:** Phase 2 退出条件满足。

**退出条件:** 「完成判据」全部勾选。

## 任务卡

### 任务卡粒度规则（强制）

沿用模板 `G-01`..`G-11`。本计划不申请任何 `EX-*` waiver。

#### 字段全局默认与例外

- **Release boundary 默认:** 每张卡独立发布切片
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `compensating`
- **Migration and rollback 默认:** `N/A：无 schema 迁移`
- **Security and privacy 默认:** 继承 GC-07、GC-11 与 GC-UID-04
- **Performance budget 默认:** 继承 GC-10 与 GC-UID-01

**默认覆盖:**

| 任务 | 偏离的字段 | 取值与理由 |
|---|---|---|
| 全部卡 | `Docs and compatibility impact` | 命令文档 EN+zh、开发文档、开发文档索引、`COMPATIBILITY.md` 与网站页面同卡同步（ER-06a） |
| `UID-03` | `Security and privacy` | Agent 内容经既有 redaction，快照不含原始 payload |
| `UID-02`/`UID-03`/`UID-04`/`UID-05` | `Full-suite trigger` | 提升共享读 API（GC-02 单一事实源）命中 `T-1` |

**规则 waiver（`EX-*`）:** 本计划无活动豁免。

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `UID-01` | implementation | 只读数据接缝、状态分类与 worker 生命周期 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 8/8 | 6/8 | 2/7 | M | DEP-UID-05 | no-overlap（链首） | independent | N/A | N/A |
| `UID-02` | implementation | History 提交历史、归属与拓扑真实化 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 8/8 | 6/8 | 4/6 | M | UID-01, DEP-UID-01 | 序列化于 UID-01 | independent | N/A | N/A |
| `UID-03` | implementation | Sessions Agent 会话真实投影与隐私边界 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 8/8 | 6/8 | 4/8 | M | UID-02, DEP-UID-01 | 序列化于 UID-02 | independent | N/A | N/A |
| `UID-04` | implementation | Operations 操作日志真实投影 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 8/8 | 6/8 | 4/5 | M | UID-03 | 序列化于 UID-03 | independent | N/A | N/A |
| `UID-05` | implementation | Memory episode 有界投影与 stale 语义 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 8/8 | 6/8 | 4/5 | M | UID-04, DEP-UID-02 | 序列化于 UID-04 | independent | N/A | N/A |
| `UID-06` | implementation | Review/Investigate run 记录投影与损坏隔离 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 8/8 | 6/8 | 3/5 | M | UID-05 | 序列化于 UID-05 | independent | N/A | N/A |
| `UID-07` | implementation | 仓库与数据面可用性状态呈现 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 8/8 | 6/8 | 3/7 | M | UID-06, DEP-UID-01, DEP-UID-02 | 序列化于 UID-06 | independent | N/A | N/A |

#### Verification 判定口径

- 每条 AC 是单一 pass/fail 谓词，且由**一个具名 test fn** 覆盖；矩阵与分类细节写在 ADR-UID-06 或 Description，不按矩阵行计数。
- 数据层测试路径为 `gui::data::tests::<fn>`；`Verification` 必须写完整 test fn 名（不是模块前缀），确保 nextest 过滤器能选中。
- 零命中守卫统一模板（`<paths>`/`<pattern>` 由各卡给出）：

  ```bash
  if rg -n '<pattern>' <paths>; then
    echo "FAIL: forbidden pattern found"; exit 1
  else
    rc=$?
    if [ "$rc" -ne 1 ]; then echo "ERROR: rg failed with exit $rc"; exit "$rc"; fi
    echo "OK: zero hits"
  fi
  ```

- 数据层生产文件不含测试代码：测试放在 `src/gui/data/tests.rs`（`#[cfg(test)] mod tests;`），守卫以 `--glob '!tests.rs'` 排除；屏幕模块保留内联测试，panic 守卫只覆盖数据层。
- 新增 test fn 标 `(new)`；不新增 `--test` target。
- 全量 nextest 不替代本卡指定用例。

### Task UID-01: 只读数据接缝、状态分类与 worker 生命周期

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 建立 `src/gui/data/` 只读接缝：用 `RequestScope::try_resolve`/`try_get_storage_path` 解析当前工作目录仓库，启动单一后台 worker（长期 runtime + scope pin + 注入连接），向 GPUI 主线程投递带序号的不可变快照与 ADR-UID-06 的源级状态。行为轴是**数据获取的线程、生命周期、只读性与失败分类**。

**Out of scope:** 各表面行数据 → `UID-02`..`UID-07`；dirty 计数 → `UID-07`；条目级状态 → `UID-03`..`UID-06`；写操作 → `DEFER-UID-01`；流式刷新 → `DEFER-UID-02`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| GPUI 在主线程运行 | `src/gui/mod.rs:30`、`src/main.rs:539` |
| `Desktop` 只有页面/选择/输入状态 | `src/gui/desktop.rs:145` |
| `storage_path()`/`working_dir()` 在仓库外 panic | `src/utils/util.rs:1269`、`:1464` |
| `RequestScope` 解析与 scope pin API | `src/internal/worktree_scope.rs:80`、`:226` |
| 连接默认允许 schema 升级；连接错误被包装 | `src/internal/db.rs:624`、`:274` |
| 无 schema 管理连接（可传 busy timeout） | `src/internal/db.rs:217` |
| 按连接检查 schema；按角色 API 拒绝 Capture | `src/internal/db/schema.rs:381`、`:877` |

**Acceptance criteria:**

- [ ] A1 仓库内启动窗口时显示来自 `RequestScope` 的仓库上下文（仓库相对工作区标签、`.libra` 存储标签、分支、HEAD），且快照由 worker 投递。
- [ ] A2 仓库外启动窗口时正常打开并显示 `absent` 源级状态，不报错退出。
- [ ] A3 全部仓库读取经单一 worker；主线程侧读取调用计数为 0。
- [ ] A4 读取后 `libra.db` 的 schema receipt 逐字节不变。
- [ ] A5 读取后 `libra.db` 的 operation head 逐字节不变。
- [ ] A6 源级状态分类按 ADR-UID-06 返回（含前置探测保留 `io::ErrorKind`），窗口按返回值渲染。
- [ ] A7 关闭窗口时 worker 在关闭预算内结束；停滞读超 deadline 时记录 `worker stalled` 并结束（surviving 线程由 handle 持有）。
- [ ] A8 关闭窗口不阻塞主线程（预算内返回）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid01_context_snapshot_in_repository` (new)
- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid01_source_state_classification` (new)
- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid01_shutdown_and_stalled` (new)
- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid01_queue_cap_and_supersession` (new)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`

**Full-suite trigger:** `none`（不动 `src/cli.rs` 命令注册与全局 flag，不改共享读 API，不新增 `--test` target）

**Dependencies:** `DEP-UID-05`

**Deliverables:** N/A

**Implementation write set:** `src/gui/data/mod.rs`、`src/gui/data/repo.rs`、`src/gui/data/worker.rs`、`src/gui/data/snapshot.rs`、`src/gui/data/state.rs`、`src/gui/data/tests.rs`、`src/gui/mod.rs`、`src/gui/desktop.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（改为"读取当前工作目录仓库，只读"）、`docs/commands/zh-CN/ui.md`（同义）、`docs/development/commands/ui.md`（更新「当前状态/还未实现的功能」）、`docs/development/commands/README.md:85`（"Needs no repository" 改为"仓库可选，窗口内只读"）、`COMPATIBILITY.md:179`（补充只读读取语义）、网站 `commands/ui.en.md`（同步）。`docs/error-codes.md` N/A（复用现有码）。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A（无 schema 迁移）。补偿：revert 提交 → 下一 patch 发布 → 网站重同步（补偿键 = 新版本号；不删除已发布 artifact）。

**Security and privacy:** 继承 GC-07/GC-11/GC-UID-04；worker 只读；上下文标签为仓库相对路径；capture 打开错误脱敏。

**Performance budget:** 上下文快照 ≤50ms（仅存在性/scope 检查，不含 dirty 扫描）；请求队列上限 8；读取 deadline ≤2s（busy timeout 同值）；关闭预算 3s。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=只读数据接缝与 worker 生命周期; recovery=compensating; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=2; prod-files=7; scope=M; deps=DEP-UID-05; writeset=no-overlap; release=independent; split-from=N/A; exception=N/A`

### Task UID-02: History 提交历史、归属与拓扑真实化

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 用真实提交历史替换 `screens/history.rs` 的静态 `COMMITS` 表，按真实归属数据（`agent_session_commit`）与真实父链重建筛选与拓扑，并移除该页残留的演示文案。行为轴是**History 表面的数据、归属与拓扑**。

**Out of scope:** diff/hunk 内容 → `DEFER-UID-06`；分支切换 UI → 沿用现有状态；提交图布局重构 → 保留绘制骨架。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| History 数据是 9 条静态常量 | `src/gui/screens/history.rs:20` |
| 筛选按作者 `eli`、图形按 `Codex` 与行号 | `src/gui/screens/history.rs:183`、`:224`、`:483` |
| 页面残留演示文案 | `src/gui/screens/history.rs:95`、`:107` |
| 归属表 | `sql/migrations/2026111601_capture_database.sql:329` |
| 归属 helper 接收注入连接但内部仍自开连接 | `src/command/agent/session.rs:763`、`:799` |
| `mod session` 私有 | `src/command/agent/mod.rs:48` |

**Acceptance criteria:**

- [ ] A1 History 显示真实提交（≤200，按提交时间倒序），不再读取 `COMMITS` 常量。
- [ ] A2 提交行显示短 OID、作者、时间与主题（主题 ≤200 字符）。
- [ ] A3 提交图按真实父链绘制，不用作者名或行号判断拓扑。
- [ ] A4 Human / Agent-authored / Has checkpoint 三个筛选由真实归属与 checkpoint 数据决定。
- [ ] A5 搜索与筛选作用于真实数据，无匹配显示空状态。
- [ ] A6 空仓库显示 `absent` 状态（"尚无提交"）。
- [ ] A7 读取只读：不写 operation 行、schema receipt 不变。
- [ ] A8 页面演示文案 "acme-libra"/"Design preview" 移除，数据层生产文件无裸 panic。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid02_history_rows` (new)
- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid02_attribution_and_graph` (new)
- [ ] `source .env.test && cargo nextest run --test agent_checkpoint_reader_test -E 'test(session_list_walks_keyset_pages)'`
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n 'COMMITS' src/gui/screens/history.rs`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`

**Full-suite trigger:** `T-1: 会话↔提交归属读取提升为双连接注入的共享 API（GC-02 共享 helper）`

**Dependencies:** `UID-01`；`DEP-UID-01`

**Deliverables:** N/A

**Implementation write set:** `src/command/agent/session.rs`、`src/command/agent/mod.rs`、`src/gui/data/history.rs`、`src/gui/data/tests.rs`、`src/gui/data/mod.rs`、`src/gui/screens/history.rs`、`src/gui/desktop.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（History 真实提交与归属）、`docs/commands/zh-CN/ui.md`（同义）、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A。补偿：revert 提交 → 新 patch 发布 → 网站重同步。

**Security and privacy:** 提交主题/作者按既有字段输出；不含绝对路径。

**Performance budget:** 提交列表 ≤150ms（≤10k 提交仓库）；显示上限 200 行；批量读取元数据，禁止逐提交 N+1。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=History 数据/归属/拓扑真实化; recovery=compensating; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=4; prod-files=6; scope=M; deps=UID-01,DEP-UID-01; writeset=序列化于 UID-01; release=independent; split-from=N/A; exception=N/A`

### Task UID-03: Sessions Agent 会话真实投影与隐私边界

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 用 `.libra/private/capture.db` 中的真实 `agent_session`/`agent_checkpoint`/`agent_bridge_event` 行替换内嵌 `sessions.json` 演示目录，并提升会话分页 SQL 为可复用读 API（连接由调用方注入，父模块导出）。行为轴是**Sessions 表面的数据来源、隐私边界与过渡消费者兼容**。

**Out of scope:** 实时捕获 → `DEFER-UID-02`；云端会话 → `DEFER-UID-04`；会话写入/编辑 → 本计划只读。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 演示目录经 `include_str!` 内嵌，另有 search/capabilities 消费者 | `src/gui/model.rs:14`、`src/gui/screens/search.rs:56`、`src/gui/screens/capabilities.rs:14` |
| capture 库路径、只读打开与错误消息 | `src/internal/ai/capture/store.rs:41`、`:93`、`:98`、`:807` |
| capture 库会话/checkpoint/事件/归属表 | `sql/migrations/2026111601_capture_database.sql:304`、`:135`、`:57`、`:329` |
| `session_page_sql` 是 `pub(super)` helper | `src/command/agent/session.rs:390` |
| `mod session` 私有 | `src/command/agent/mod.rs:48` |
| 文件会话存储 `load()` 会迁移旧格式 | `src/internal/ai/session/store.rs:29`、`:377` |

**Acceptance criteria:**

- [ ] A1 Sessions 显示 `capture.db` 真实会话（≤200，按 `started_at` 倒序），不再读取 `sessions.json`。
- [ ] A2 会话详情显示真实 agent、模型、起止时间、usage、checkpoint 数与 `agent_session_commit` 链接行；缺失字段显示"—"。
- [ ] A3 会话内容经既有 redaction 后显示，单条 payload 截断 ≤4 KiB。
- [ ] A4 搜索与"可读内容"过滤作用于真实数据，无结果显示空状态。
- [ ] A5 `capture.db` 未创建显示 `absent` 状态；不可读/损坏按 ADR-UID-06 源级状态显示（错误消息脱敏）。
- [ ] A6 读取只读：`libra.db` 与 `capture.db` 的 schema receipt 不变，且不写 operation 行。
- [ ] A7 过渡接口明确：`model::catalog()` 仅保留给 `search`/`capabilities`（Sessions 不再调用），`UID-07` 移除或标注过渡。
- [ ] A8 数据层生产文件无裸 `unwrap(`/`expect(`/`panic!(`，且 GUI 不构造 `SessionStore`。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid03_session_rows` (new)
- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid03_redaction_and_payload_cap` (new)
- [ ] `source .env.test && cargo nextest run --test agent_checkpoint_reader_test -E 'test(session_list_walks_keyset_pages)'`
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n 'sessions\.json' src/gui --glob '*.rs'`
- [ ] 零命中守卫（模板三分支）：`rg -n 'SessionStore' src/gui --glob '*.rs'`

**Full-suite trigger:** `T-1: 把 `session_page_sql` 与取行提升为命令层与 GUI 共用的单一事实源（GC-02 共享 helper）`

**Dependencies:** `UID-02`；`DEP-UID-01`

**Deliverables:** N/A

**Implementation write set:** `src/command/agent/session.rs`、`src/command/agent/mod.rs`、`src/gui/data/sessions.rs`、`src/gui/data/tests.rs`、`src/gui/data/mod.rs`、`src/gui/view.rs`、`src/gui/model.rs`、`src/gui/sessions.json`（删除）、`src/gui/desktop.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（Sessions 真实捕获与 redaction 边界、过渡接口）、`docs/commands/zh-CN/ui.md`（同义）、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A（只读）。补偿：revert 提交（恢复 `sessions.json`）→ 新 patch 发布 → 网站重同步。

**Security and privacy:** Agent 内容经既有 redaction；快照不含原始 payload；绝对 `working_dir` 与打开错误路径转换为仓库相对路径；不读取未注册 agent 的私有目录。

**Performance budget:** 会话列表 ≤200ms（≤200 会话，走既有分页索引）；详情 ≤100ms（≤100 事件）；显示上限 200 行；payload 单条 ≤4 KiB。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=Sessions 真实会话投影与隐私边界; recovery=compensating; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=4; prod-files=8; scope=M; deps=UID-02,DEP-UID-01; writeset=序列化于 UID-02; release=independent; split-from=N/A; exception=N/A`

### Task UID-04: Operations 操作日志真实投影

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 用 operation v2 journal 替换 Operations 页面静态卡片，并提升命令层私有的分页查询为可复用读 API（保持 CLI 输出不变、连接注入）。行为轴是**Operations 表面的分页数据投影**。

**Out of scope:** 撤销/恢复执行 → `DEFER-UID-01`；统计聚合口径 → `docs/development/internal/stats.md`；memory 投影 → `UID-05`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| operation v2 存储 API；head 按 scope 记录 | `src/internal/operation/store.rs:330`、`:949` |
| 历史查询是命令层私有 helper（ranked CTE + COUNT + OFFSET，按可变 `end_ts` 排序） | `src/command/op.rs:642`、`:651`、`:662`、`:693` |
| CLI 分页从第 1 页开始 | `src/command/op.rs:251` |
| 子命令是 `op log` | `src/command/op.rs:42` |

**Acceptance criteria:**

- [ ] A1 Operations 显示真实 operation 行（≤200，按时间倒序），不再显示静态卡片。
- [ ] A2 每行显示 kind、时间、状态与摘要（摘要 ≤200 字符）。
- [ ] A3 命令过滤与搜索作用于真实数据，无匹配显示空状态。
- [ ] A4 空仓库显示 `absent` 状态；不可读/损坏按 ADR-UID-06 源级状态显示。
- [ ] A5 分页用 revision token（全 scope head 集 + 行数 + max `end_ts`，与页面同事务读取）判定稳定性；token 变化时重置到第 1 页，避免 offset 重复/丢行。
- [ ] A6 读取只读：不写 operation 行、schema receipt 不变。
- [ ] A7 提升后 CLI `libra op log` 输出与既有测试逐字节一致。
- [ ] A8 数据层生产文件无裸 `unwrap(`/`expect(`/`panic!(`。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid04_operation_rows` (new)
- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid04_pagination_revision_token` (new)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::op_test)'`
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`
- [ ] 手工（macOS 桌面会话）：仓库内执行一次写操作后打开窗口，验证行、过滤、翻页与空状态

**Full-suite trigger:** `T-1: 把 `query_operation_log_page` 提升为命令层与 GUI 共用的单一事实源（GC-02 共享 helper）`

**Dependencies:** `UID-03`

**Deliverables:** N/A

**Implementation write set:** `src/command/op.rs`、`src/gui/data/operations.rs`、`src/gui/data/tests.rs`、`src/gui/data/mod.rs`、`src/gui/screens/operations.rs`、`src/gui/desktop.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（Operations 数据来源与 revision token 语义）、`docs/commands/zh-CN/ui.md`（同义）、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A。补偿：revert 提交 → 新 patch 发布 → 网站重同步。

**Security and privacy:** 摘要不含绝对路径、不含原始 payload。

**Performance budget:** 单页 ≤150ms；显示上限 200 行/页；查询成本 = 既有 ranked CTE 对仓库 operation 历史的排序 + COUNT（与 CLI 相同，不是 O(200)），在 ≥10k operation 仓库上实测并记录；不引入新索引。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=Operations 分页投影; recovery=compensating; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=4; prod-files=5; scope=M; deps=UID-03; writeset=序列化于 UID-03; release=independent; split-from=N/A; exception=N/A`

### Task UID-05: Memory episode 有界投影与 stale 语义

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 用 memory episode 只读投影替换 Memory 页面静态卡片，并新增**伴生**有界读 API（保留 `list_episodes` 原签名，CLI 与投影调用方不变），把 episode 行与子表证据/路径限制在本页选中的 episode 集合。freshness 校验保留既有**全源指纹**语义（成本如实登记）。行为轴是**Memory 表面的有界投影与 stale 语义**。

**Out of scope:** `libra memory rebuild` 执行 → 本计划只读；投影算法与 horizon 口径 → `plan-20260926`；统计聚合 → `docs/development/internal/stats.md`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `list_episodes` 无 limit，子表按 repo 全量取 | `src/internal/ai/memory/reader.rs:198`、`:315` |
| freshness 用全源 operation 指纹 | `src/internal/ai/memory/reader.rs:165`、`src/internal/ai/memory/projection.rs:183` |
| memory 过期 fail-closed 语义 | `src/command/memory.rs` 顶部注释（ADR-DM-10） |

**Acceptance criteria:**

- [ ] A1 Memory 显示真实 episode 行（≤200，按投影顺序），不再显示静态卡片。
- [ ] A2 每行显示 episode 短 id、时间、来源类型与摘要（摘要 ≤200 字符）。
- [ ] A3 投影过期时显示 `stale` 状态（提示 `libra memory rebuild`），不显示陈旧行。
- [ ] A4 搜索与来源过滤作用于真实数据，无结果显示空状态。
- [ ] A5 空投影显示 `absent` 状态；不可读/损坏按 ADR-UID-06 源级状态显示。
- [ ] A6 伴生 API 只读本页选中的 episode 集合（episode 行与子表按选中集合过滤）。
- [ ] A7 既有 `list_episodes` 签名与调用方（`libra memory`、投影）行为不变。
- [ ] A8 读取只读：不写 operation 行、不触发 rebuild、schema receipt 不变。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid05_episode_rows` (new)
- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid05_bounded_children_and_stale` (new)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::memory_test)'`
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`
- [ ] 手工（macOS 桌面会话）：仓库内 `libra memory rebuild` 后打开窗口，验证行、过期提示与空状态

**Full-suite trigger:** `T-1: 在 `src/internal/ai/memory/` 新增共享有界读 API（GC-02 单一事实源）`

**Dependencies:** `UID-04`；`DEP-UID-02`

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/memory/reader.rs`、`src/gui/data/memory.rs`、`src/gui/data/tests.rs`、`src/gui/data/mod.rs`、`src/gui/screens/memory.rs`、`src/gui/desktop.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（Memory 数据来源、stale 语义与 freshness 成本）、`docs/commands/zh-CN/ui.md`（同义）、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A。补偿：revert 提交 → 新 patch 发布 → 网站重同步。

**Security and privacy:** episode 摘要不含绝对路径、不含原始 payload。

**Performance budget:** 单页 ≤150ms（≤200 选中 episode；子表按选中集合过滤）；freshness 保留全源校验成本（在 ≥10k episode 仓库上实测并记录）；显示上限 200 行。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=Memory 有界投影与 stale 语义; recovery=compensating; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=4; prod-files=5; scope=M; deps=UID-04,DEP-UID-02; writeset=序列化于 UID-04; release=independent; split-from=N/A; exception=N/A`

### Task UID-06: Review/Investigate run 记录投影与损坏隔离

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 用 `.libra/sessions/agent-runs/<run_id>/` 的真实 run 目录替换 Reviews/Investigate 页面静态卡片：全目录扫描后按时间取最新 100，逐项用**打开句柄的有界读**（≤64 KiB）解析公开 wire 类型，使损坏/超大/缺件行成为条目级状态而不是整页失败。行为轴是**运行记录面的逐项读取与损坏隔离**。

**Out of scope:** 触发新 run → `DEFER-UID-01`；run 全文 → `DEFER-UID-07`；run 清理 → 本计划只读。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| run 目录与 wire 文件 | `src/internal/ai/review/mod.rs:6` |
| 列表 API 跳过不可解析项；逐项读取用无上限 `fs::read` | `src/internal/ai/review/store.rs:882`、`:1226` |
| 公开 wire 类型与逐项读取 API | `src/internal/ai/review/store.rs:513`、`src/internal/ai/investigate/store.rs:629` |
| 屏幕静态 run 字面量 | `src/gui/screens/reviews.rs:31`、`src/gui/screens/investigate.rs:29` |

**Acceptance criteria:**

- [ ] A1 Reviews/Investigate 显示真实 run（最新 100，按时间倒序），不再显示静态 run 字面量。
- [ ] A2 选中 run 时显示轮次、参与 agent 与结论摘要（摘要 ≤200 字符）。
- [ ] A3 损坏 `state.json` 只影响该行（条目级 `malformed`），其余行仍可用。
- [ ] A4 产物缺件（缺 `manifest.json`）显示条目级 `incomplete`。
- [ ] A5 超过 64 KiB 的文件显示条目级 `oversized`（有界读在反序列化前拒绝超额字节）。
- [ ] A6 目录扫描上限 5000；超过时列表标记 `truncated`；run 目录未创建显示 `absent`，不可读按 ADR-UID-06 源级状态显示。
- [ ] A7 读取只读：不获取 run 锁写权限、不创建目录、不写 operation 行。
- [ ] A8 数据层生产文件无裸 `unwrap(`/`expect(`/`panic!(`。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid06_run_rows` (new)
- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid06_item_states_and_limits` (new)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n '\b(rv|iv)-[0-9a-f]{6}\b' src/gui/screens/reviews.rs src/gui/screens/investigate.rs`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`
- [ ] 手工（macOS 桌面会话）：仓库内造 1 条正常 run、1 条损坏 run、1 条超大 run，验证隔离、truncated 与空状态

**Full-suite trigger:** `none`（只消费既有 wire 类型与逐项读取，不改共享读 API）

**Dependencies:** `UID-05`

**Deliverables:** N/A

**Implementation write set:** `src/gui/data/runs.rs`、`src/gui/data/tests.rs`、`src/gui/data/mod.rs`、`src/gui/screens/reviews.rs`、`src/gui/screens/investigate.rs`、`src/gui/desktop.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（run 记录来源、条目级状态、扫描/字节上限）、`docs/commands/zh-CN/ui.md`（同义）、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A。补偿：revert 提交 → 新 patch 发布 → 网站重同步。

**Security and privacy:** run 摘要经既有 redaction；不含绝对路径；不含未脱敏 transcript。

**Performance budget:** 列表 ≤300ms（全目录扫描 ≤5000 条 metadata + 最新 100 × ≤64 KiB 有界读）；详情 ≤50ms（`state.json`/`manifest.json` 各自 ≤64 KiB）。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=run 记录逐项投影与损坏隔离; recovery=compensating; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=3; prod-files=5; scope=M; deps=UID-05; writeset=序列化于 UID-05; release=independent; split-from=N/A; exception=N/A`

### Task UID-07: 仓库与数据面可用性状态呈现

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 让 Splash、Welcome、About 与 Capability map 反映真实仓库与数据面可用性：显示仓库是否已打开、分支/HEAD/dirty 摘要、各数据源（capture、memory、operation、runs、history）的状态，替换固定文案与插图，并把窗口标签改为 "Read-only preview"。行为轴是**状态与可用性呈现**。

**Out of scope:** 导入/克隆/初始化执行 → `DEFER-UID-01`；多仓库列表 → `DEFER-UID-03`；远端/云可用性 → `DEFER-UID-04`；其余演示页 → `DEFER-UID-05`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Splash 固定文案 | `src/gui/screens/splash.rs:67` |
| Welcome 预览不打开/不创建仓库 | `src/gui/screens/welcome.rs:1` |
| About 固定 "not connected" | `src/gui/screens/about.rs:69` |
| Capability map 依赖演示目录 | `src/gui/screens/capabilities.rs:14` |
| 窗口 "Design preview" 标签 | `src/gui/desktop.rs:332` |
| dirty 摘要来源（工作树扫描） | `src/command/status.rs:1145` |

**Acceptance criteria:**

- [ ] A1 Splash 显示真实仓库名（工作目录名）、分支、HEAD 短值、对象格式与 dirty 计数（staged+unstaged+untracked 合计）；仓库外显示 `absent`。
- [ ] A2 About 显示真实仓库状态与各数据源状态（可用/缺失/不可读/损坏/不兼容），不再显示固定 "not connected"。
- [ ] A3 Capability map 按真实可用性标注五个数据面，缺失项显示 ADR-UID-06 中的状态。
- [ ] A4 Welcome 在仓库内显示当前仓库摘要，仓库外保留导入/克隆/打开入口且不执行动作。
- [ ] A5 窗口标签由 "Design preview" 改为 "Read-only preview"，并同步文档。
- [ ] A6 状态读取只读：不写 operation 行、不推进 schema、不创建仓库。
- [ ] A7 `catalog()` 演示依赖在本卡移除或标注过渡，删除后无死代码警告。
- [ ] A8 数据层生产文件无裸 `unwrap(`/`expect(`/`panic!(`。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid07_context_and_availability` (new)
- [ ] `source .env.test && cargo nextest run --lib gui::data::tests::uid07_dirty_count` (new)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n 'acme-libra|not connected|Design preview' src/gui/screens/splash.rs src/gui/screens/welcome.rs src/gui/screens/about.rs src/gui/screens/capabilities.rs src/gui/desktop.rs`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`
- [ ] 手工（macOS 桌面会话）：仓库内/外分别打开 Splash、About、Capability、Welcome，记录状态差异、dirty 摘要与标签文案

**Full-suite trigger:** `none`（复用 `RequestScope`/`try_get_storage_path` 与既有状态读取，不改共享读 API）

**Dependencies:** `UID-06`；`DEP-UID-01`；`DEP-UID-02`

**Deliverables:** N/A

**Implementation write set:** `src/gui/data/context.rs`、`src/gui/data/tests.rs`、`src/gui/data/mod.rs`、`src/gui/screens/splash.rs`、`src/gui/screens/welcome.rs`、`src/gui/screens/about.rs`、`src/gui/screens/capabilities.rs`、`src/gui/desktop.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（状态页、可用性矩阵与标签文案）、`docs/commands/zh-CN/ui.md`（同义）、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A。补偿：revert 提交 → 新 patch 发布 → 网站重同步。

**Security and privacy:** 只显示仓库相对路径与分支名；不显示宿主绝对路径。

**Performance budget:** 可用性探测 ≤80ms（存在性/可打开检查）；dirty 摘要 ≤250ms（≤5k 文件 stat 扫描）。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=仓库与数据面可用性呈现; recovery=compensating; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=3; prod-files=7; scope=M; deps=UID-06,DEP-UID-01,DEP-UID-02; writeset=序列化于 UID-06; release=independent; split-from=N/A; exception=N/A`

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | 仓库上下文、worker supersession/队列/关闭/stalled、源级状态分类 | `source .env.test && cargo nextest run --lib gui::data::tests::uid01_` |
| 单元 | 各表面行、归属、分页 revision token、redaction、条目级状态 | `source .env.test && cargo nextest run --lib gui::data::tests::uid0` |
| 集成 | `libra ui` CLI 表面不变 | `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'` |
| 集成 | session 列表分页契约（提升后的共享 SQL） | `source .env.test && cargo nextest run --test agent_checkpoint_reader_test -E 'test(session_list_walks_keyset_pages)'` |
| 集成 | 提升后的读 API 不改变既有命令输出 | `source .env.test && cargo nextest run --test command_test -E 'test(command::op_test) or test(command::memory_test)'` |
| 兼容 | 文档与 `COMPATIBILITY.md` 同步守卫 | `python3 scripts/sync-libra-command-docs.py --libra-root ../libra --only ui`（后端仓库） |
| 安全 | 无裸 panic（数据层）、无绝对路径、redaction、不构造 `SessionStore` | `rg` 零命中守卫（模板三分支，排除 `tests.rs`） |
| 性能 | 上限截断与实测成本记录 | 数据层单测中的规模断言（≤200 行/≤300ms/≥10k 规模实测） |
| live/gated | 窗口人工验收（需显示器） | 手工：`./target/debug/libra ui` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| `UID-01` | `src/gui/mod.rs:30`、`src/utils/util.rs:1269`、`src/internal/db.rs:217`、`src/internal/db/schema.rs:381` | `src/gui/data/{mod,repo,worker,snapshot,state}.rs`、`src/gui/mod.rs`、`src/gui/desktop.rs` | `docs/commands/ui.md` EN+zh、开发文档、开发文档索引、`COMPATIBILITY.md`、网站 | `uid01_context_snapshot_in_repository`、`uid01_source_state_classification`、`uid01_shutdown_and_stalled` (new) |
| `UID-02` | `src/gui/screens/history.rs:20`、`:224`、`:95`、`sql/migrations/2026111601_capture_database.sql:329` | `src/command/agent/{session,mod}.rs`、`src/gui/data/history.rs`、`src/gui/screens/history.rs` | 同上 | `uid02_history_rows`、`uid02_attribution_and_graph` (new) |
| `UID-03` | `src/gui/model.rs:14`、`src/command/agent/session.rs:390`、`src/internal/ai/capture/store.rs:93` | `src/command/agent/{session,mod}.rs`、`src/gui/data/sessions.rs`、`src/gui/view.rs`、`src/gui/model.rs` | 同上（含 redaction 与过渡接口） | `uid03_session_rows`、`uid03_redaction_and_payload_cap` (new) |
| `UID-04` | `src/command/op.rs:662`、`src/internal/operation/store.rs:949` | `src/command/op.rs`、`src/gui/data/operations.rs`、`src/gui/screens/operations.rs` | 同上（revision token） | `uid04_operation_rows`、`uid04_pagination_revision_token` (new) |
| `UID-05` | `src/internal/ai/memory/reader.rs:198`、`:315`、`:165` | `src/internal/ai/memory/reader.rs`、`src/gui/data/memory.rs`、`src/gui/screens/memory.rs` | 同上（stale 与 freshness 成本） | `uid05_episode_rows`、`uid05_bounded_children_and_stale` (new) |
| `UID-06` | `src/internal/ai/review/mod.rs:6`、`src/internal/ai/review/store.rs:1226` | `src/gui/data/runs.rs`、`src/gui/screens/reviews.rs`、`src/gui/screens/investigate.rs` | 同上（条目级状态与上限） | `uid06_run_rows`、`uid06_item_states_and_limits` (new) |
| `UID-07` | `src/gui/screens/splash.rs:67`、`src/gui/screens/about.rs:69`、`src/gui/desktop.rs:332` | `src/gui/data/context.rs`、四个屏幕、`src/gui/desktop.rs` | 同上（可用性状态与标签） | `uid07_context_and_availability`、`uid07_dirty_count` (new) |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 基线冻结、计划 R4 review PASS、维护者授权 | 计划文件签名提交、`plan-status.md` 行 | N/A |
| M1 | `UID-01` `done`/`complete` | patch 发布、`release.yml` success、D 组绿 | 发布前：revert 提交；发布后：补偿发布 + 网站重同步 |
| M2 | `UID-02`..`UID-07` 全部 `done`/`complete` | 六次独立发布与 D 组证据 | 同上逐卡补偿 |
| M3 | 计划完成判据全部满足 | 全量收口门绿、计划状态同步；**收口时做一次补偿演练**（revert 一项已发布卡并重发/重同步） | 前滚修复 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| worker 启动后窗口打开前失败 | 无 | 重开窗口或显示状态 | 主线程 panic |
| 读取中途数据库被外部迁移 | 无（不写） | 显示 `schema-old`/`schema-new`，用户用 CLI 升级 | 自动迁移或静默降级 |
| 窗口关闭时 worker 仍在读 | 已投递旧快照（丢弃） | 置取消标志；预算内等待；超 deadline 记录 `worker stalled`，surviving 线程由 handle 持有 | 无限等待、主线程阻塞或线程泄漏无主 |
| `capture.db` 未创建 | 无 | 显示 `absent` | 回退演示数据 |
| `capture.db` 不可读/权限拒绝 | 无 | 显示 `unreadable`（前置探测保留 io kind，消息脱敏） | 把不可读当空状态 |
| 库结构损坏 | 无 | 显示 `corrupt` | 把损坏当空状态 |
| run 目录损坏/超大 | 该行 `malformed`/`oversized` | 逐项隔离，其余可用 | 整页失败 |
| 目录扫描超过 5000 | 列表标记 `truncated` | 显示最新 100 行 | 静默截断 |
| 快照序号乱序 | 无 | 丢弃过期响应并重发 | 用旧数据覆盖新数据 |
| 已发布版本/网站回滚需求 | 已发布 artifact 保留 | revert 提交 + 新 patch 发布 + 网站重同步（补偿键=新版本号） | 删除已发布 artifact 或强推 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| GPUI 主线程被 I/O 阻塞导致窗口冻结 | 高：用户以为程序卡死 | GC-UID-01 + worker 单测（含停滞模拟）+ 手工验收 | `UID-01` |
| 只读面触发 schema 升级或 operation 行 | 高：污染仓库状态 | ADR-UID-02 + 无 schema 管理连接 + 读后状态不变判据 | `UID-01` |
| GUI 复制命令逻辑导致语义漂移 | 中：显示错误事实 | ADR-UID-03 + 提升共享读 API + CLI 输出回归 | `UID-02`..`UID-05` |
| Agent 内容泄漏 secret 或绝对路径 | 高：隐私暴露 | ADR-UID-04 + redaction 复用 + 路径转换 + 脱敏 | `UID-03` |
| 演示数据兜底掩盖读取失败 | 中：用户看到假数据 | GC-UID-05 + ADR-UID-06 + 过渡接口边界 | 全部 |
| 与 20261117 批次并发改共享文档 | 中：冲突/覆盖 | ER-12 文件级串行窗口 + 精确暂存 | 全部 |
| 大仓库读取超预算 | 中：窗口迟滞 | 行/字节/目录上限 + 实测成本记录（不做虚假 O(200) 承诺） | `UID-02`..`UID-06` |
| `storage_path()`/`get_db_conn_instance()` panic 路径被误用 | 高：窗口崩溃 | 只用 `RequestScope::try_resolve`/`try_get_storage_path`/注入连接 | `UID-01` |
| 提升共享读 API 破坏 CLI 输出 | 高：兼容回归 | 同卡 CLI 输出回归测试 + `T-1` 全量门 | `UID-02`..`UID-05` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 仓库上下文快照 | O(1) 存在性/scope 检查 | 每窗口一次 | ≤50ms | `UID-01` 单测 |
| 提交历史列表 | O(200 提交元数据 + 父链) | 每次页面重读 | ≤150ms、200 行 | `UID-02` 单测 |
| 会话列表/详情 | O(200 会话)、O(100 事件) | 每次页面重读 | ≤200ms / ≤100ms | `UID-03` 单测 |
| operation 单页 | ranked CTE 对仓库历史排序 + COUNT（既有成本，非 O(200)） | 每次翻页 | ≤150ms；≥10k operation 实测记录 | `UID-04` 单测 |
| memory 单页 | O(200 选中 episode) + 选中集合子表 + 全源 freshness | 每次页面重读 | ≤150ms；≥10k episode 实测记录 | `UID-05` 单测 |
| run 列表/详情 | 全目录扫描 ≤5000 metadata + 最新 100 × ≤64 KiB 有界读 | 每次页面重读 | ≤300ms / ≤50ms、100 行 | `UID-06` 单测 |
| 可用性探测 | O(5 数据面) | 每窗口一次 | ≤80ms | `UID-07` 单测 |
| dirty 摘要 | O(工作树 stat，≤5k 文件) | 每窗口一次 | ≤250ms | `UID-07` 单测 |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已同步，或说明 `N/A`。
- [ ] `docs/commands/ui.md` 已同步，或说明 `N/A`。
- [ ] `docs/commands/zh-CN/ui.md` 已同步，或说明 `N/A`。
- [ ] `docs/development/commands/ui.md` 已同步，或说明 `N/A`。
- [ ] `docs/development/commands/README.md` 已同步，或说明 `N/A`。
- [ ] `docs/error-codes.md` 已同步，或说明 `N/A`。
- [ ] `tests/INDEX.md` 已同步，或说明 `N/A`（不新增 `--test` target）。
- [ ] `Cargo.toml` `[[test]]` 已同步，或说明 `N/A`。
- [ ] `plan-long.md` 日期计划索引已登记本计划，或说明 `N/A`。

## Codex review log

Result 只允许 `PASS` 或 `FAIL`。`FAIL` 必须列出 P0/P1 条目并在下一轮复审关闭。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 | 全文（首版，SHA-256 `286ed2943ff417e25024b8d441fd714d27abe510d7cc2cb6ecb015fd4ad3197d`） | FAIL | Codex：P0=0；P1=7（迁移默认开启、`SessionStore` 副作用、无界读、History 演示语义、D 组缺 `release.yml`、focused nextest 未覆盖全部表面、网站不可写误记 `remote-pending`、worker 关闭无取消边界）。Claude：P0=0；P1=7（AC 捆绑、写集漏共享 API 提升且 `T-1` 误写 `none`、写集相交未入图、未登记 `ui` 修复依赖） | 修订：拆卡、AC 逐谓词、补 `T-1`、补依赖边、补 `release.yml`、补 `compensating`、补状态矩阵、补守卫命令、修正锚点与计数 | Codex exec 与 Claude Code 只读全文审查（2026-11-20 18:0x UTC） |
| R2 | 全文（第二版，SHA-256 `eea3da953ed91a58986ffbb5317b94acc0c5f303431be4639ae599409c69bb6b`） | FAIL | Codex：P0=0；P1=7（归属 helper 迁移注入、关闭边界不可执行、memory 子查询无界、op CTE 成本非 O(200)、offset 翻页重复、run 目录/文档无上限、缺 session CLI focused 门）。Claude：P0=0；P1=6（AC 计数口径、写集漏调用方/`tests.rs`、守卫不可命中或不可通过、目录消费者三处矛盾、D 组未枚举 job、DEP 粒度/等待上限） | 修订：注入连接 + busy timeout、scope pin、memory 伴生 API、op 真实成本 + head pin、run 目录/字节上限、源级/条目级状态分离、D 组枚举 job、守卫具体模式、`op log`、相对路径、prod-file 计数、DEP 粒度、补偿演练收口一次 | Codex exec 与 Claude Code 只读全文审查（2026-11-20 19:0x UTC） |
| R3 | 全文（第三版，SHA-256 `4ab80bbebf9d4c785022b3deb1595ee252b5adc642489175a1c00fde7385eb1a`） | FAIL | Codex：P0=0；P1=8（归属仍自开连接、关闭 ADR/AC 冲突、memory 页内 freshness 与全源指纹冲突、op head 不足以稳定 offset、run 字节上限仅预检、schema 检查绕过注入、权限错误被 `is_file()` 抹掉、AC→test 映射与 `gui::data::tests::*` 路径）。Claude：P0=0；P1=5（写集漏 `agent/mod.rs`、UID-01 计数、状态矩阵三缺口、`open_connection_without_schema_management` 也是读写打开的误述、op head scope/页码/事务、run 扫描与守卫正则、History 残留文案无归属、DEP 消费者与 README 窗口、演示目录过渡矛盾、缺「非目标与延后项」「完成判据」两节） | 修订：双连接注入并移除内部 open；补 `agent/mod.rs`；关闭 ADR/AC 对齐；freshness 保留全源成本；op revision token；run 打开句柄有界读 + 全目录扫描 + truncated；schema 用按连接 API + Capture 自身 `schema_versions`；权限前置探测；AC 拆分并给具名 test fn；守卫正则；DEP 消费者/README 窗口/页面数；补回两节 | Codex exec 与 Claude Code 只读全文审查（2026-11-20 20:0x UTC） |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-UID-01 | 窗口内写操作（restore/trust/rebase/checkout/导入/克隆/触发 run/memory rebuild） | 本计划只读；写操作需要 writer boundary 与确认语义 | 只读数据面稳定且维护者立项 | 未来 `libra ui` 写操作计划（不登记 outgoing DEP） |
| DEFER-UID-02 | 实时流式刷新（watcher/增量推送） | 快照模式已足够；避免线程与背压复杂度 | 用户要求实时更新 | 未来刷新计划 |
| DEFER-UID-03 | 多仓库标签页与 `libra ui <path>` | 需要路径参数与状态隔离 | 多仓需求确认 | 未来多仓计划 |
| DEFER-UID-04 | 远端/云数据（Mega2 traces、Cloud 快照） | 需要网络、认证与缓存策略 | 本地只读面稳定 | 未来云端数据计划 |
| DEFER-UID-05 | 其余 16 个演示页：refs、agents、automation、bisect、blame、conflict、deps、lanes、rebase、search、settings、views、merge review、cloud、mega2 browser、splash error | 本计划只接六个数据表面 + 一个状态面 | 演示目录消费者全部迁移后 | 未来逐面接入计划 |
| DEFER-UID-06 | diff/hunk 内容视图 | 只接提交元数据与拓扑 | 需要 hunk 级审阅 | 未来 diff 面计划 |
| DEFER-UID-07 | run 全文（findings/diff 内容） | 只显示摘要 | 需要审阅 run 细节 | 未来 run 详情计划 |
| DEFER-UID-08 | Linux/Windows 桌面人工验收 | 验收平台为 macOS | 有对应桌面会话或图形 CI | 未来跨平台验收 |

## 完成判据

计划只有在以下条件全部满足后才能标记完成：

- [ ] 所有任务卡满足粒度规则 `G-*`：无未登记的 L 例外、无 XL 卡、无碎片卡、无未登记的合并发布例外、实现写集冲突均已消解；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务的 acceptance criteria 已满足，且 `Lifecycle=done` **且** `Acceptance=complete`。
- [ ] 所有任务的 Verification 命令已运行并记录结果。
- [ ] **计划完成门 / 全量收口门（ER-13）**：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿。
- [ ] **全量收口门暴露的 Bug 已全部处理**：本计划改动导致的失败已前滚修复并重跑全量至全绿；既有失败附基线复现证据并登记 `FIX-*`/`DEFER-*`。
- [ ] 必要的 docs/compat/error-code/test-index 更新已完成（含网站 `cf` 交付、Claude Code PASS 与部署检查）。
- [ ] 每张卡的 `Rollback mode` 已被实际验证（收口时一次补偿演练；无法验证时记录原因）。
- [ ] Codex（或维护者指定 reviewer）最终结论为 `PASS`，P0/P1 全部关闭。
- [ ] 每张卡已 `patch + 1`、版本面集合一致、`Cargo.lock` 仅由工具链刷新、`gh release create` 触发 `release.yml` 并取得 `base.yml`/`codeql.yml`/`release.yml` 的 D 组证据。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
- [ ] `plan-status.md` 的计划行与 §五/§六 登记、`plan-long.md` 日期计划索引已同步。
